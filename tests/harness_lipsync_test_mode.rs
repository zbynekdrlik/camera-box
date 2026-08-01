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
