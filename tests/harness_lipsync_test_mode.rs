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
