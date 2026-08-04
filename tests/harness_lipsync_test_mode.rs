//! issue 930 — `scripts/lipsync-test-mode.sh`'s pure remote-command builders (kill the TEST-mode
//! painter by pidfile, launch the lipsync ffmpeg playback, kill it again), sourced and called
//! directly. NEVER touches cam2 or the network — mirrors rig-mode.sh's own
//! painter_launch_remote/painter_stop_remote convention and the harness style already
//! established for it (`tests/harness_cam2_painter_provisioning_863.rs`).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lipsync-test-mode.sh")
}

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn run_sourced(call: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$1\"; {call}"))
        .arg("bash")
        .arg(script())
        .output()
        .expect("spawn bash");
    assert!(
        out.status.success(),
        "sourced call `{call}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn lipsync_test_mode_script_exists_and_is_executable() {
    let meta = fs::metadata(script())
        .unwrap_or_else(|e| panic!("scripts/lipsync-test-mode.sh missing: {e}"));
    assert!(meta.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "scripts/lipsync-test-mode.sh must be executable"
        );
    }
}

/// The painter-stop command MUST key on the SAME pidfile constant rig-mode.sh's own TEST-mode
/// painter uses (`/run/rig-painter.pid` by default) -- a mismatch would silently target the
/// wrong (or no) process. Never a bare `pkill -f frame-probe` (would also match this very ssh
/// command's own cmdline -- the exact discipline rig-mode.sh's painter_stop_remote documents).
#[test]
fn stop_painter_cmds_kills_by_pidfile_never_pkill_by_name() {
    let cmds = run_sourced("lipsync_stop_painter_cmds /run/rig-painter.pid");
    assert!(cmds.contains("/run/rig-painter.pid"));
    assert!(cmds.contains("kill"));
    assert!(
        !cmds.contains("pkill"),
        "930: must kill by pidfile, never a name-based pkill (self-match risk): {cmds}"
    );

    let rig_mode = read("scripts/rig-mode.sh");
    assert!(
        rig_mode.contains("PAINTER_PIDFILE=\"${PAINTER_PIDFILE:-/run/rig-painter.pid}\""),
        "930: lipsync-test-mode.sh's default PAINTER_PIDFILE must match rig-mode.sh's own \
         constant -- it is stopping/restoring the SAME painter process"
    );
    let this_script = read("scripts/lipsync-test-mode.sh");
    assert!(
        this_script.contains("PAINTER_PIDFILE=\"${PAINTER_PIDFILE:-/run/rig-painter.pid}\""),
        "930: this script's default PAINTER_PIDFILE must stay byte-identical to rig-mode.sh's"
    );
}

/// The playback command must feed ONE ffmpeg process both `/dev/fb0` (video, bgra pixel format
/// matching src/probe/fb.rs's own painter convention) AND the ALSA marker device (audio, -ac 2 --
/// the live sanity test found the device refuses mono) from a SINGLE demux/decode timeline (one
/// `-i`, two `-map`s) -- never two separate processes that could drift out of sync with each
/// other.
#[test]
fn playback_cmds_feeds_one_ffmpeg_process_both_sinks() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /root/lipsync-test.mp4 /dev/fb0 hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    // "nohup ffmpeg" only appears on the ACTUAL invocation line (the failure-message text below
    // also mentions the word "ffmpeg" in prose, so counting bare "ffmpeg" substrings would be
    // wrong -- anchor on the real command form instead).
    let ffmpeg_invocations = cmds.matches("nohup ffmpeg").count();
    assert_eq!(
        ffmpeg_invocations, 1,
        "930: exactly ONE ffmpeg invocation (single demux/decode timeline), not two \
         processes that could drift apart: {cmds}"
    );
    assert!(cmds.contains("-map 0:v"));
    assert!(cmds.contains("-map 0:a"));
    assert!(cmds.contains("-pix_fmt bgra"));
    assert!(cmds.contains("-f fbdev"));
    assert!(cmds.contains("/dev/fb0"));
    assert!(cmds.contains("-f alsa"));
    assert!(cmds.contains("hw:CARD=PCH,DEV=3"));
    assert!(
        cmds.contains("-ac 2"),
        "930: -ac 2 needed -- the live sanity test found the ALSA device refuses mono: {cmds}"
    );
    assert!(
        cmds.contains("-stream_loop -1"),
        "930: must loop -- the ~60s asset must cover an arbitrary-length recording window: {cmds}"
    );
    assert!(cmds.contains("/run/rig-lipsync-playback.pid"));
}

/// The playback command must FAIL LOUD (not silently proceed) if ffmpeg dies immediately after
/// launch -- mirrors rig-mode.sh's own painter-liveness verification convention (never claim a
/// launch succeeded without checking the process is actually alive).
#[test]
fn playback_cmds_fails_loud_if_ffmpeg_dies_immediately() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /root/lipsync-test.mp4 /dev/fb0 hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        cmds.contains("kill -0") && cmds.contains("FAIL"),
        "930: must verify the launched pid is actually alive and FAIL loud if not: {cmds}"
    );
}

/// End-to-end pacing-guard fakes (930 finding 9) -- a fake `ffprobe` reports a fixed duration and
/// a fake `ffmpeg` sleeps a controlled number of seconds for the ONE-SHOT preflight pass. Kept as
/// its OWN `lipsync_pacing_guard_cmd` function (never folded into `lipsync_playback_cmds`) so
/// this never touches the persistent launch's `/run/*.pid`/`/run/*.log` paths, which need a real
/// root/remote session and would otherwise fail with a permission error under a non-root test
/// runner (incl. CI's `ubuntu-latest`). Proves the guard's pass/fail behavior for real, not just
/// via a text-pattern check on the source.
fn run_pacing_guard(duration_secs: u32, sleep_secs: u32) -> std::process::Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("ffprobe"),
        format!("#!/usr/bin/env bash\necho {duration_secs}\n"),
    )
    .unwrap();
    fs::write(
        bin.join("ffmpeg"),
        // Emits ONE showinfo-style line before sleeping -- a real ffmpeg with -vf showinfo
        // always prints at least the first frame's line once it starts producing output, so a
        // fake that emits ZERO frames on a clean exit is unrealistic and (930 review finding)
        // would spuriously trip the "showinfo produced nothing" check added below. A single
        // frame is not enough to form any delta (needs a PAIR), so this keeps these two tests
        // exercising ONLY the elapsed-vs-duration budget check, exactly as before.
        "#!/usr/bin/env bash\necho '[Parsed_showinfo_0 @ 0x0] n:   0 pts:     0 pts_time:0.000000' >&2\nsleep \"${FAKE_FFMPEG_SLEEP:-0}\"\n",
    )
    .unwrap();
    for exe in [bin.join("ffprobe"), bin.join("ffmpeg")] {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    Command::new("bash")
        .arg("-c")
        .arg(
            ". \"$1\"; eval \"$(lipsync_pacing_guard_cmd /tmp/media.mp4 /dev/fb0 hw:CARD=PCH,DEV=3)\"",
        )
        .arg("bash")
        .arg(script())
        .env("PATH", path_env)
        .env("FAKE_FFMPEG_SLEEP", sleep_secs.to_string())
        .output()
        .expect("spawn bash")
}

/// A genuinely EMPTY fake `ffmpeg` -- exits 0, prints NOTHING to stderr at all. Dedicated to
/// proving the "showinfo produced zero parseable frames" guard (930 review finding): a real
/// ffmpeg always prints at least one showinfo line if it ran and processed any frames, so zero
/// lines on a clean exit means the instrumentation itself broke (e.g. ffmpeg's log format
/// changed) -- NOT a verified pacing pass, and must not be silently reported as one.
fn run_pacing_guard_zero_frames(duration_secs: u32, sleep_secs: u32) -> std::process::Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("ffprobe"),
        format!("#!/usr/bin/env bash\necho {duration_secs}\n"),
    )
    .unwrap();
    fs::write(
        bin.join("ffmpeg"),
        "#!/usr/bin/env bash\nsleep \"${FAKE_FFMPEG_SLEEP:-0}\"\n",
    )
    .unwrap();
    for exe in [bin.join("ffprobe"), bin.join("ffmpeg")] {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    Command::new("bash")
        .arg("-c")
        .arg(
            ". \"$1\"; eval \"$(lipsync_pacing_guard_cmd /tmp/media.mp4 /dev/fb0 hw:CARD=PCH,DEV=3)\"",
        )
        .arg("bash")
        .arg(script())
        .env("PATH", path_env)
        .env("FAKE_FFMPEG_SLEEP", sleep_secs.to_string())
        .output()
        .expect("spawn bash")
}

#[test]
fn pacing_guard_fails_loud_when_showinfo_produces_zero_frames_930() {
    // Same duration/sleep as pacing_guard_passes_within_budget_930 below -- the elapsed check
    // alone is satisfied, but zero showinfo lines were ever observed, so the guard must refuse
    // to certify a pacing pass it never actually verified.
    let out = run_pacing_guard_zero_frames(2, 2);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "930: zero showinfo frames on a clean exit must FAIL -- the cadence was never actually \
         verified, so this must not read as a pass: {stderr}"
    );
    assert!(
        stderr.contains("FAIL") && stderr.contains("showinfo") && stderr.contains("ZERO"),
        "930: failure message must say showinfo produced zero frames, not just report a \
         (meaningless) cadence/elapsed verdict: {stderr}"
    );
}

/// A fake `ffprobe` that FAILS (nonzero exit, a message on stderr, nothing usable on stdout) --
/// proves ffprobe failures (930 review finding) surface as a clean FAIL message quoting ffprobe's
/// own stderr, not an uncaught Python traceback with no context about which command failed.
#[test]
fn pacing_guard_fails_loud_with_a_clean_message_when_ffprobe_itself_fails_930() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("ffprobe"),
        "#!/usr/bin/env bash\necho 'moov atom not found' >&2\nexit 1\n",
    )
    .unwrap();
    fs::write(bin.join("ffmpeg"), "#!/usr/bin/env bash\nsleep 0\n").unwrap();
    for exe in [bin.join("ffprobe"), bin.join("ffmpeg")] {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            ". \"$1\"; eval \"$(lipsync_pacing_guard_cmd /tmp/media.mp4 /dev/fb0 hw:CARD=PCH,DEV=3)\"",
        )
        .arg("bash")
        .arg(script())
        .env("PATH", path_env)
        .output()
        .expect("spawn bash");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "930: a failing ffprobe must fail the guard: {stderr}"
    );
    assert!(
        stderr.contains("FAIL") && stderr.contains("ffprobe"),
        "930: must be a clean FAIL message naming ffprobe as the cause, not an uncaught \
         Python traceback with no context: {stderr}"
    );
    assert!(
        stderr.contains("moov atom not found"),
        "930: must surface ffprobe's OWN stderr so the real cause is visible: {stderr}"
    );
    assert!(
        !stderr.contains("Traceback"),
        "930: must not leak a raw Python traceback to the operator: {stderr}"
    );
}

#[test]
fn pacing_guard_passes_within_budget_930() {
    let out = run_pacing_guard(2, 2);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "930: expected pass: {stderr}");
    // The pass-path "ok: ..." line is a plain `echo` (stdout) -- only the FAIL path is `>&2`'d.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("pacing check passed"),
        "930: stdout: {stdout} / stderr: {stderr}"
    );
}

#[test]
fn pacing_guard_fails_loud_when_elapsed_exceeds_budget_930() {
    let out = run_pacing_guard(2, 5);
    assert!(
        !out.status.success(),
        "930: must fail loud when elapsed (~5s) vs duration (2s) exceeds the ~1.01s budget"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FAIL") && stderr.contains("pacing"),
        "930: must fail loud with elapsed + duration in the message: {stderr}"
    );
}

/// 930 follow-up (pacing hypothesis, confirmed by direct measurement): without `-re` (real-time
/// input read rate), `/dev/fb0` has no clock of its own and ALSA backpressure alone does NOT pace
/// the video -- frames were measured arriving in 4-5-frame bursts ~4ms apart separated by ~80ms
/// stalls. `-re` fixes this (measured: p50=16.663ms std=1.7ms, zero stalls, drift <1ms/40s) and
/// must be present in BOTH ffmpeg invocations that actually play the asset.
#[test]
fn pacing_guard_cmd_includes_dash_re_for_realtime_pacing_930() {
    let cmds = run_sourced("lipsync_pacing_guard_cmd /tmp/media.mp4 /dev/fb0 hw:CARD=PCH,DEV=3");
    assert!(
        cmds.contains("-re"),
        "930: the pacing guard's ffmpeg invocation must read the input in REAL TIME (-re) -- \
         without it, /dev/fb0's lack of its own clock means ALSA backpressure alone does not \
         pace the video (measured: 4-5-frame bursts ~4ms apart, ~80ms stalls): {cmds}"
    );
}

#[test]
fn playback_cmds_includes_dash_re_for_realtime_pacing_930() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /root/lipsync-test.mp4 /dev/fb0 hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        cmds.contains("-re"),
        "930: the PERSISTENT playback ffmpeg invocation must also read in real time (-re) -- \
         same pacing bug as the guard, and this is what actually plays during a recording: {cmds}"
    );
}

/// 930 follow-up: the OLD guard only checked TOTAL elapsed wall-clock vs `ffprobe`'s reported
/// duration -- and that total is governed by the AUDIO drain, so it passed even while the video
/// was catastrophically unpaced (see the two tests above). The guard must ALSO instrument the
/// same foreground pass with `-vf showinfo` and assert per-frame cadence, so a pacing regression
/// can never again hide behind a total-elapsed-only budget. Static-text pin on the thresholds
/// (the functional proof is in `pacing_guard_catches_bursty_cadence_...` below).
#[test]
fn pacing_guard_cmd_asserts_per_frame_cadence_930() {
    let cmds = run_sourced("lipsync_pacing_guard_cmd /tmp/media.mp4 /dev/fb0 hw:CARD=PCH,DEV=3");
    assert!(
        cmds.contains("showinfo"),
        "930: the guard must instrument the SAME foreground pass with -vf showinfo to observe \
         per-frame delivery timing: {cmds}"
    );
    assert!(
        cmds.contains("33.0"),
        "930: stall threshold (deltas >33ms) must be visible in the guard's own text: {cmds}"
    );
    assert!(
        cmds.contains("4.0"),
        "930: burst threshold (deltas <4ms) must be visible in the guard's own text: {cmds}"
    );
    assert!(
        cmds.contains("5.0"),
        "930: p95-deviation-from-nominal threshold (5ms) must be visible in the guard's own \
         text: {cmds}"
    );
    assert!(
        cmds.contains("0.02"),
        "930: burst-fraction threshold (2% of deltas) must be visible in the guard's own text: \
         {cmds}"
    );
    assert!(
        cmds.contains("LIPSYNC_PACING_STARTUP_SKIP_S"),
        "930: the one-time startup step must be excludable via an env override, default 2s: \
         {cmds}"
    );
}

/// Fakes for the CADENCE assertion (930 follow-up: per-frame pacing, not just total elapsed) --
/// a fake `ffprobe` answers BOTH the pre-existing duration query and the (new) fps query, and a
/// fake `ffmpeg` (itself a tiny python3 script, for precise controllable timing) prints
/// synthetic showinfo-style frame lines to stderr on a controlled real-time cadence.
/// `pattern="bursty"` reproduces the EXACT shape measured live on cam2 without `-re` (930
/// comment): frame bursts a few ms apart separated by ~100ms stalls, with a TOTAL elapsed time
/// that still lands well within the pre-existing elapsed-vs-duration budget -- proving the OLD
/// check alone would have PASSED this broken pattern, and only the NEW cadence assertion catches
/// it.
fn run_pacing_guard_cadence(pattern: &str, startup_skip_s: &str) -> std::process::Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("ffprobe"),
        "#!/usr/bin/env bash\ncase \"$*\" in\n  *r_frame_rate*) echo \"60/1\" ;;\n  *) echo \"0.333333\" ;;\nesac\n",
    )
    .unwrap();
    fs::write(
        bin.join("ffmpeg"),
        r#"#!/usr/bin/env python3
import os
import sys
import time

pattern = os.environ.get("FAKE_FFMPEG_PATTERN", "steady")
n = 20


def emit(i):
    sys.stderr.write(
        "[Parsed_showinfo_0 @ 0x0] n:{:4} pts:{:6} pts_time:{:.6f}\n".format(i, i, i / 60.0)
    )
    sys.stderr.flush()


if pattern == "bursty":
    i = 0
    while i < n:
        for _ in range(4):
            if i >= n:
                break
            time.sleep(0.001)
            emit(i)
            i += 1
        time.sleep(0.1)
elif pattern == "crash":
    # Emits a FULL, cleanly-paced run (same cadence as "steady") so elapsed/cadence alone would
    # look perfectly fine -- then exits nonzero AFTER all frames, simulating e.g. a late
    # fbdev/alsa write failure. This is the genuine false-pass scenario: without a returncode
    # check, a crash-after-good-data run would be silently reported as a PASS.
    for i in range(n):
        time.sleep(1.0 / 60.0)
        emit(i)
    sys.exit(3)
else:
    for i in range(n):
        time.sleep(1.0 / 60.0)
        emit(i)

sys.exit(0)
"#,
    )
    .unwrap();
    for exe in [bin.join("ffprobe"), bin.join("ffmpeg")] {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    Command::new("bash")
        .arg("-c")
        .arg(
            ". \"$1\"; eval \"$(lipsync_pacing_guard_cmd /tmp/media.mp4 /dev/fb0 hw:CARD=PCH,DEV=3)\"",
        )
        .arg("bash")
        .arg(script())
        .env("PATH", path_env)
        .env("FAKE_FFMPEG_PATTERN", pattern)
        .env("LIPSYNC_PACING_STARTUP_SKIP_S", startup_skip_s)
        .output()
        .expect("spawn bash")
}

#[test]
fn pacing_guard_catches_bursty_cadence_when_elapsed_check_alone_would_pass_930() {
    let out = run_pacing_guard_cadence("bursty", "0");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "930: a bursty pacing pattern (frame bursts a few ms apart, ~100ms stalls) must FAIL the \
         cadence assertion even though its total elapsed time stays well within the old \
         elapsed-vs-duration budget -- this is exactly the gap the old guard could not see: \
         {stderr}"
    );
    assert!(
        stderr.contains("FAIL") && stderr.contains("cadence"),
        "930: must fail loud with cadence evidence in the message: {stderr}"
    );
    assert!(
        stderr.contains("stalls"),
        "930: failure message must show the stall count that tripped it: {stderr}"
    );
}

#[test]
fn pacing_guard_excludes_startup_window_from_cadence_assertion_930() {
    // A skip window larger than the whole (synthetic, sub-second) clip means EVERY frame falls
    // inside the excluded startup window -- zero deltas are asserted, so the guard falls back to
    // the elapsed-vs-duration check alone and PASSES even for the same bursty fake that fails
    // pacing_guard_catches_bursty_cadence_when_elapsed_check_alone_would_pass_930 above. Proves
    // the startup-skip config (LIPSYNC_PACING_STARTUP_SKIP_S, default 2s in production) actually
    // excludes what it claims to exclude.
    let out = run_pacing_guard_cadence("bursty", "999");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "930: a startup skip window covering the whole clip must exclude all deltas from the \
         cadence assertion, leaving only the (passing) elapsed check: {stderr}"
    );
}

/// Independent code review finding: every other cadence test exercises the FAILING path (bursty)
/// or a vacuous n==0 path (the skip-window test above) -- nothing ever proved the cadence math
/// (p95 computation, the nominal-interval comparison, the threshold constants) actually reports a
/// PASS with real n>0 delta data. A sign flip or a wrong operator in that arithmetic would go
/// completely undetected by the rest of this file. Uses the "steady" fake pattern (real 1/60s
/// python time.sleep() per frame, no ffmpeg/device overhead involved -- unlike the real-hardware
/// pacing measurement in the design comment on issue 930, this is a lightweight, short (~330ms
/// total) synthetic timing loop, chosen for a low flake footprint).
#[test]
fn pacing_guard_passes_steady_cadence_with_real_delta_data_930() {
    let out = run_pacing_guard_cadence("steady", "0");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "930: a cleanly-paced steady stream must PASS the cadence assertion: stdout={stdout} \
         stderr={stderr}"
    );
    assert!(
        stdout.contains("pacing check passed") && stdout.contains("deltas="),
        "930: the pass message must show real cadence data was computed (deltas=N, N>0), not \
         just the elapsed-only budget: {stdout}"
    );
    assert!(
        !stdout.contains("deltas=0"),
        "930: this test's whole point is a NON-vacuous cadence verdict -- deltas=0 would mean \
         no real delta data backed this PASS: {stdout}"
    );
}

/// Self-review finding (before merge): the probe read `proc.stderr` for showinfo lines and
/// computed a pass/fail verdict from whatever partial frame data it collected, WITHOUT ever
/// checking ffmpeg's own exit code -- a genuine crash (e.g. `/dev/fb0` open failure) could leave
/// a coincidentally-plausible partial dataset and report a false PASS instead of the real
/// failure. The probe must check `proc.returncode` and fail loud on a nonzero exit, independent
/// of whatever cadence/elapsed numbers happened to be observed before the crash.
#[test]
fn pacing_guard_fails_loud_when_ffmpeg_itself_exits_nonzero_930() {
    let out = run_pacing_guard_cadence("crash", "0");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "930: ffmpeg exiting nonzero must fail the guard regardless of any partial frame data \
         collected before the crash: {stderr}"
    );
    assert!(
        stderr.contains("FAIL") && stderr.contains("ffmpeg exited"),
        "930: failure message must say the ffmpeg process itself failed, not just report a \
         cadence/elapsed verdict over incomplete data: {stderr}"
    );
}

/// `cmd_start` must run the pacing guard on the uploaded remote path via its OWN dedicated
/// `lipsync_pacing_guard_cmd` call, before the persistent playback launch -- a static-text pin
/// alongside the end-to-end behavior proofs above.
#[test]
fn start_runs_the_pacing_guard_before_the_persistent_playback_930() {
    let s = read("scripts/lipsync-test-mode.sh");
    let guard_call_at = s
        .find("cam_ssh \"$(lipsync_pacing_guard_cmd")
        .expect("930: cmd_start must call lipsync_pacing_guard_cmd");
    let playback_call_at = s
        .find("cam_ssh \"$(lipsync_playback_cmds")
        .expect("lipsync_playback_cmds call present");
    assert!(
        guard_call_at < playback_call_at,
        "930: the pacing guard must run BEFORE the persistent playback launch"
    );
}

/// The stop-playback command must key on the SAME pidfile the start command wrote.
#[test]
fn stop_playback_cmds_kills_by_the_same_pidfile() {
    let cmds = run_sourced("lipsync_stop_playback_cmds /run/rig-lipsync-playback.pid");
    assert!(cmds.contains("/run/rig-lipsync-playback.pid"));
    assert!(cmds.contains("kill"));
}

/// `stop` must call rig-mode.sh's OWN `test` mode to restore -- never a hand-rolled partial
/// restore (the acceptance criterion: "TEST mode restored and verified after every run").
#[test]
fn stop_subcommand_calls_rig_mode_sh_test_to_restore() {
    let s = read("scripts/lipsync-test-mode.sh");
    assert!(
        s.contains("rig-mode.sh") && s.contains("rig-mode.sh\" test"),
        "930: stop must restore via `rig-mode.sh test` (full re-verified restore), never a \
         hand-rolled partial one: {s}"
    );
}

/// `cmd_start` must set an ERR trap (with `errtrace` enabled so it fires even for a failure
/// inside a called function like `cam_ssh`) restoring TEST mode via `rig-mode.sh test` -- a
/// scp/ssh failure between killing the TEST-mode painter and starting the lipsync playback must
/// never leave cam2 with NEITHER the QR/QPSK painter NOR the lipsync playback running (930
/// finding 8). The trap must be cleared once `cmd_start` completes successfully, so a later
/// unrelated failure elsewhere in the script doesn't also trigger it.
#[test]
fn start_sets_an_err_trap_that_restores_test_mode_930() {
    let s = read("scripts/lipsync-test-mode.sh");
    assert!(
        s.contains("set -o errtrace"),
        "930: errtrace needed so the ERR trap fires for a failure inside a called function \
         (cam_ssh), not just a bare command: {s}"
    );
    assert!(
        s.contains(r#"trap 'bash "$HERE/rig-mode.sh" test' ERR"#),
        "930: cmd_start must set an ERR trap that restores TEST mode via rig-mode.sh: {s}"
    );
    assert!(
        s.contains("trap - ERR"),
        "930: the ERR trap must be cleared once cmd_start completes successfully: {s}"
    );
    // The trap must be set AFTER the painter is already killed (the window it protects) and
    // cleared BEFORE the function's final success message -- never wrapping the whole function.
    let set_at = s.find("set -o errtrace").expect("errtrace present");
    let kill_at = s
        .find("cam_ssh \"$(lipsync_stop_painter_cmds")
        .expect("painter kill present");
    let clear_at = s.find("trap - ERR").expect("trap clear present");
    assert!(
        kill_at < set_at && set_at < clear_at,
        "930: ERR trap must be scoped between the painter kill and the success clear"
    );
}

#[test]
fn main_with_unknown_subcommand_fails_loud() {
    let out = Command::new("bash")
        .arg(script())
        .arg("bogus")
        .output()
        .expect("spawn bash");
    assert!(!out.status.success());
}

#[test]
fn start_with_a_missing_media_file_fails_loud_before_touching_the_network() {
    let out = Command::new("bash")
        .arg(script())
        .arg("start")
        .arg("/nonexistent/path/does-not-exist.mp4")
        .output()
        .expect("spawn bash");
    assert!(
        !out.status.success(),
        "930: a missing asset must fail BEFORE any ssh/scp attempt (no network in this test env, \
         so a hang/network-error here would mean the file-existence check was skipped)"
    );
}
