//! issue 1194 — `scripts/lipsync-test-mode.sh`'s ERR/fail path must KILL the mpv playback BEFORE the
//! rig-mode.sh TEST restore. A restore that re-launches the cam2 painter while an mpv `--vo=drm`
//! playback still holds the DRM master + ALSA `hw:CARD=PCH,DEV=3` produces the live-incident hybrid
//! state (painter dead on `snd_pcm_open ... busy (16)` + card->fbdev fallback, mpv alive). The fix:
//! extract ONE idempotent `lipsync_playback_cleanup` helper (kill mpv by pidfile + blank fb0 + drop
//! the /run asset), shared by `cmd_stop` AND the `cmd_start` ERR trap, and run it FIRST in both — a
//! mirror of the issue-1190 start-side ordering (unit-stop BEFORE the pidfile kill).
//!
//! Two layers, per `.claude/rules/ci-testing-gotchas.md` (#1133 — static anchors alone are blind to
//! a `set -e` ordering regression):
//!   * STATIC anchors — the helper exists, BOTH call sites use it (no re-inlined copy), and the ERR
//!     trap body runs the cleanup before the restore.
//!   * FUNCTIONAL replica — the real script + its sourced libs run in a hermetic tempdir whose
//!     siblings are FAKES (a fake `sshpass` on PATH that emits an ordering sentinel when it carries
//!     the mpv-kill payload — the #660 `dd if=/dev/zero` fb0-blank signature no other `cmd_start`
//!     cam_ssh carries — and a fake `rig-mode.sh` that emits a restore sentinel). It proves, through
//!     the script's OWN `set -euo pipefail`+errtrace control flow, that the mpv-kill command is
//!     ISSUED before the restore stub on the exhaustion path, the infra-fail path, AND on `stop`.
//!     (The real remote kill on cam2 is a supervisor rig step; here we prove the ORDERING the fix
//!     guarantees, which is exactly what the OLD trap violated.)

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_script() -> String {
    let path = manifest_dir().join("scripts/lipsync-test-mode.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// --------------------------------------------------------------------------------------------- //
// STATIC anchors.
// --------------------------------------------------------------------------------------------- //

/// The fix's core: ONE idempotent `lipsync_playback_cleanup` helper must exist and be CALLED from
/// BOTH `cmd_stop` and the `cmd_start` ERR trap -- one source of truth, never a re-inlined copy.
#[test]
fn shared_playback_cleanup_helper_exists_and_both_sites_call_it_1194() {
    let s = read_script();
    assert!(
        s.contains("lipsync_playback_cleanup() {"),
        "1194: a single idempotent lipsync_playback_cleanup helper must be defined: {s}"
    );
    // 1 definition + >= 2 call sites (cmd_stop + the ERR trap).
    let occurrences = s.matches("lipsync_playback_cleanup").count();
    assert!(
        occurrences >= 3,
        "1194: the cleanup helper must be CALLED from both cmd_stop and the ERR trap (definition + \
         >= 2 calls); found {occurrences} occurrences of the name: {s}"
    );
}

/// `cmd_stop` must delegate teardown to the shared helper, NOT re-inline the playback-stop cam_ssh
/// (the "one truth, no re-inlined copy" requirement). Scoped to cmd_stop's own body.
#[test]
fn cmd_stop_uses_the_shared_cleanup_helper_not_an_inline_copy_1194() {
    let s = read_script();
    let start = s.find("cmd_stop() {").expect("cmd_stop present");
    let end = s[start..]
        .find("\nmain() {")
        .map(|i| start + i)
        .unwrap_or(s.len());
    let body = &s[start..end];
    assert!(
        body.contains("lipsync_playback_cleanup"),
        "1194: cmd_stop must call the shared lipsync_playback_cleanup helper: {body}"
    );
    // The positive assertion above (cmd_stop calls the helper) is the load-bearing "one source of
    // truth" check; this negative check is the belt that specifically guards against RE-INLINING the
    // playback-stop cam_ssh back into cmd_stop.
    assert!(
        !body.contains("lipsync_stop_playback_cmds"),
        "1194: cmd_stop must NOT re-inline the playback-stop cam_ssh -- it belongs in the shared \
         helper (one source of truth): {body}"
    );
    // The full re-verified restore via rig-mode.sh test must still be the last thing cmd_stop does.
    assert!(
        body.contains("rig-mode.sh\" test"),
        "1194: cmd_stop must still restore TEST mode via rig-mode.sh test after the cleanup: {body}"
    );
}

/// Inside the `cmd_start` ERR trap, the playback cleanup must PRECEDE the rig-mode restore -- the
/// whole point of #1194. The exact new trap string is pinned; the ordering is then re-checked within
/// the trap body. Against the OLD `trap 'bash "$HERE/rig-mode.sh" test' ERR` this `.find()` returns
/// None and the test fails (the genuine RED).
#[test]
fn err_trap_cleans_up_playback_before_restoring_test_mode_1194() {
    let s = read_script();
    let trap_at = s
        .find("trap 'lipsync_playback_cleanup; bash \"$HERE/rig-mode.sh\" test' ERR")
        .expect("1194: the cmd_start ERR trap must clean up the mpv playback BEFORE the rig-mode restore");
    // The trap must sit AFTER errtrace is enabled (the window it protects) -- unchanged from #930.
    let errtrace_at = s.find("set -o errtrace").expect("errtrace present");
    assert!(
        errtrace_at < trap_at,
        "1194: the ERR trap must be set after errtrace (unchanged #930 window): {s}"
    );
    // Within the trap body itself, cleanup must come before the restore.
    let tail = &s[trap_at..];
    let cleanup_off = tail
        .find("lipsync_playback_cleanup")
        .expect("cleanup in trap body");
    let restore_off = tail
        .find("rig-mode.sh\" test")
        .expect("restore in trap body");
    assert!(
        cleanup_off < restore_off,
        "1194: inside the ERR trap, the playback cleanup must precede the rig-mode restore: {}",
        &tail[..cleanup_off.max(restore_off) + 40]
    );
}

// --------------------------------------------------------------------------------------------- //
// FUNCTIONAL replica -- the hermetic rig (mirrors tests/harness_lipsync_arrival_verify_1192.rs).
// --------------------------------------------------------------------------------------------- //

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
}

/// Build the hermetic rig: the real script + its three sourced libs, plus the fakes. The fake
/// `sshpass` emits an ordering sentinel (`PLAYBACK-CLEANUP-KILL`) whenever the ssh it stands in for
/// carries the playback-stop payload -- recognized by the #660 `dd if=/dev/zero` fb0-blank text that
/// `lipsync_stop_playback_cmds` embeds and NO other `cmd_start` cam_ssh carries (stop_painter,
/// preflight, playback-launch, asset-rm, and the win_ssh_run delete all lack it). The fake
/// `rig-mode.sh` emits the restore sentinel. So the combined output's ordering of the two sentinels
/// is exactly "was the mpv playback killed before TEST mode was restored?".
fn setup(dir: &Path) {
    let src = manifest_dir();
    fs::create_dir_all(dir.join("lib")).unwrap();
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::copy(
        src.join("scripts/lipsync-test-mode.sh"),
        dir.join("lipsync-test-mode.sh"),
    )
    .unwrap();
    for lib in [
        "rig-test-ledger.sh",
        "win-ssh-exec.sh",
        "audio-presence-preflight.sh",
    ] {
        fs::copy(src.join("scripts/lib").join(lib), dir.join("lib").join(lib)).unwrap();
    }
    // Fake rig-mode.sh -- the restore target; prints a sentinel we assert on.
    write_exec(
        &dir.join("rig-mode.sh"),
        "#!/usr/bin/env bash\nset -euo pipefail\necho \"RIG-MODE-TEST-RESTORE $*\"\nexit 0\n",
    );
    // Fake obs_phase2.py -- record start = exit 0; stop = print a fake Windows path (or nothing on
    // the infra-fail branch, when OBS_FAKE_STOP_EMPTY=1).
    fs::write(
        dir.join("obs_phase2.py"),
        "import os, sys\n\
         argv = sys.argv\n\
         action = argv[argv.index('--action') + 1] if '--action' in argv else ''\n\
         if action == 'stop' and os.environ.get('OBS_FAKE_STOP_EMPTY') != '1':\n\
         \x20   print(r'D:\\_REC\\probe-arrival.mp4')\n\
         sys.exit(0)\n",
    )
    .unwrap();
    // Fake lipsync_envelope_corr.py -- prints corr=<Nth of CORR_SEQ>, incrementing a counter file.
    fs::write(
        dir.join("lipsync_envelope_corr.py"),
        "import os, sys\n\
         seq = os.environ.get('CORR_SEQ', '0.9').split()\n\
         cf = os.environ['CORR_COUNTER']\n\
         try:\n\
         \x20   n = int(open(cf).read().strip())\n\
         except Exception:\n\
         \x20   n = 0\n\
         open(cf, 'w').write(str(n + 1))\n\
         print('corr=' + seq[min(n, len(seq) - 1)])\n\
         sys.exit(0)\n",
    )
    .unwrap();
    // Fake sshpass on PATH. NEVER executes the remote payload (a real `dd if=/dev/zero of=/dev/fb0`
    // would wipe THIS box's framebuffer) -- it only INSPECTS the payload for the mpv-kill signature
    // and emits an ordering sentinel. For scp (win_ssh_download) it touches the local dest so the
    // probe file exists; for ssh it succeeds after the sentinel check.
    write_exec(
        &dir.join("bin/sshpass"),
        "#!/usr/bin/env bash\n\
         if [ \"${3:-}\" = \"scp\" ]; then touch \"${!#}\" 2>/dev/null || true; exit 0; fi\n\
         case \"${!#}\" in\n\
         \x20 *\"dd if=/dev/zero\"*) echo \"PLAYBACK-CLEANUP-KILL\" ;;\n\
         esac\n\
         exit 0\n",
    );
    fs::write(dir.join("asset.mp4"), b"dummy").unwrap();
}

fn path_with_bin(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Run `lipsync-test-mode.sh start` in the hermetic rig with a scripted corr sequence.
fn run_start(corr_seq: &str, retries: &str, stop_empty: bool) -> Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let dir = tmp.path().to_path_buf();
    setup(&dir);
    let mut cmd = Command::new("bash");
    cmd.arg(dir.join("lipsync-test-mode.sh"))
        .arg("start")
        .arg(dir.join("asset.mp4"))
        .env("PATH", path_with_bin(&dir))
        .env("CORR_SEQ", corr_seq)
        .env("CORR_COUNTER", dir.join("corr_counter"))
        .env("LIPSYNC_ARRIVAL_RETRIES", retries)
        .env("LIPSYNC_ARRIVAL_PROBE_S", "0")
        .env("LIPSYNC_ARRIVAL_READ_RETRY_SLEEP", "0")
        .env("LIPSYNC_ARRIVAL_SSH_TIMEOUT", "10")
        .env("STREAM", "1.2.3.4")
        .env("STREAM_USER", "x")
        .env("STREAM_PW", "x")
        .env("PAINTER_IP", "1.2.3.4")
        .env("CAM_PW", "x")
        .env("OBS_PASSWORD", "");
    if stop_empty {
        cmd.env("OBS_FAKE_STOP_EMPTY", "1");
    }
    cmd.output().expect("spawn bash")
}

/// Run `lipsync-test-mode.sh stop` in the hermetic rig (no arrival env needed).
fn run_stop() -> Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let dir = tmp.path().to_path_buf();
    setup(&dir);
    Command::new("bash")
        .arg(dir.join("lipsync-test-mode.sh"))
        .arg("stop")
        .env("PATH", path_with_bin(&dir))
        .env("PAINTER_IP", "1.2.3.4")
        .env("CAM_PW", "x")
        .output()
        .expect("spawn bash")
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Assert the mpv-kill was issued BEFORE the rig-mode restore in the combined output.
fn assert_kill_before_restore(all: &str) {
    let kill_at = all.find("PLAYBACK-CLEANUP-KILL").unwrap_or_else(|| {
        panic!(
            "1194: the mpv playback must be killed (fb0-blanking stop) before the restore: {all}"
        )
    });
    let restore_at = all
        .find("RIG-MODE-TEST-RESTORE")
        .unwrap_or_else(|| panic!("the run must restore TEST mode via rig-mode.sh test: {all}"));
    assert!(
        kill_at < restore_at,
        "1194: the mpv playback cleanup must be issued BEFORE the rig-mode restore (else the \
         restored painter races the surviving mpv for DRM+ALSA): {all}"
    );
}

/// Exhaustion path (retries=1, corr always below threshold): the ERR trap fires with NO prior
/// recycle, so a playback-cleanup here can ONLY come from the trap's own cleanup. Against the OLD
/// bare-restore trap there is no `PLAYBACK-CLEANUP-KILL` at all -> the genuine RED.
#[test]
fn exhaustion_kills_playback_before_restore_1194() {
    let out = run_start("0.3", "1", false);
    let all = combined(&out);
    assert!(
        !out.status.success(),
        "exhaustion must exit nonzero (fail loud): {all}"
    );
    assert!(
        all.contains("asset speech never reached mbc after 1 attempts"),
        "exhaustion must fail loud naming the arrival failure: {all}"
    );
    assert_kill_before_restore(&all);
    assert!(
        !all.contains("RESULT: lipsync-test mode ACTIVE"),
        "exhaustion must never print a silent ACTIVE: {all}"
    );
}

/// Infra-fail path (StopRecord returns no path -> `false` before any recycle): the ERR trap must
/// ALSO kill the playback before restoring. Same discriminator, a different entry into the trap.
#[test]
fn infra_failure_kills_playback_before_restore_1194() {
    let out = run_start("0.9", "4", true);
    let all = combined(&out);
    assert!(
        !out.status.success(),
        "an infra failure must exit nonzero: {all}"
    );
    assert!(
        all.contains("StopRecord returned no path"),
        "must fail loud on the empty StopRecord path: {all}"
    );
    assert_kill_before_restore(&all);
}

/// A passing arrival (corr >= threshold on attempt 1) reaches ACTIVE, exits 0, and does NOT restore
/// TEST mode -- the cleanup helper must not fire on the success path (no ERR, trap cleared).
#[test]
fn success_path_does_not_clean_up_or_restore_1194() {
    let out = run_start("0.9", "4", false);
    let all = combined(&out);
    assert!(out.status.success(), "a passing arrival must exit 0: {all}");
    assert!(
        all.contains("RESULT: lipsync-test mode ACTIVE"),
        "a passing arrival must reach ACTIVE: {all}"
    );
    assert!(
        !all.contains("RIG-MODE-TEST-RESTORE"),
        "the success path must NOT restore TEST mode (ERR trap never fired): {all}"
    );
}

/// `stop` still kills the playback before restoring, after the refactor to the shared helper -- the
/// cleanup helper is exercised through cmd_stop too, not only the trap.
#[test]
fn stop_kills_playback_before_restore_via_shared_helper_1194() {
    let out = run_stop();
    let all = combined(&out);
    assert!(out.status.success(), "stop must exit 0: {all}");
    assert_kill_before_restore(&all);
}
