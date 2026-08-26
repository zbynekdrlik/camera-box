//! issue 1192 — FUNCTIONAL test of `scripts/lipsync-test-mode.sh` `cmd_start`'s speech-arrival
//! VERIFY + retry ORCHESTRATION loop, run under the script's own real `set -euo pipefail`+errtrace.
//!
//! WHY a functional test on top of the static anchors in `tests/harness_lipsync_test_mode.rs`:
//! the ~60-line record->pull->correlate->retry->recycle->exhaustion block is the riskiest, most
//! `set -e`-sensitive part, and `.claude/rules/ci-testing-gotchas.md` (#1133) is explicit that a
//! static-anchor-only / `set -uo`-only test is STRUCTURALLY BLIND to a `set -e` abort — a future
//! edit could introduce a silent mid-loop abort (or a broken fail-loud+restore) with every anchor
//! test still green. So this drives the FOUR real outcomes through the actual control flow:
//!   1. corr >= threshold on attempt 1 -> ACTIVE, exit 0, TEST mode NOT restored;
//!   2. low corr then high -> recycle mpv once, then ACTIVE, exit 0;
//!   3. all retries low -> FAIL LOUD with the attempt matrix, TEST mode restored via the ERR trap,
//!      exit nonzero, no ACTIVE;
//!   4. an infra failure (StopRecord returns no path) -> FAIL LOUD, TEST mode restored, exit nonzero.
//!
//! It is HERMETIC: the real script + its sourced libs are copied into a tempdir whose siblings are
//! FAKES — a fake `obs_phase2.py` (record start/stop), a fake `lipsync_envelope_corr.py` (prints a
//! scripted corr per attempt), a fake `rig-mode.sh` (the ERR-trap restore target, prints a sentinel),
//! and a fake `sshpass` on PATH (neutralizes cam2 ssh + win_ssh_download/win_ssh_run). No cam2, no
//! stream OBS, no network. `LIPSYNC_ARRIVAL_PROBE_S=0` + `..._READ_RETRY_SLEEP=0` keep it instant.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
}

/// Build the hermetic rig in a fresh tempdir: the real script + its three sourced libs, plus the
/// four fakes. Returns the tempdir (kept alive by the caller) and the script path.
fn setup(dir: &Path) {
    let src = manifest_dir();
    fs::create_dir_all(dir.join("lib")).unwrap();
    fs::create_dir_all(dir.join("bin")).unwrap();
    // Real script + real (pure) sourced libs.
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
    // Fake rig-mode.sh — the ERR-trap restore target; prints a sentinel we assert on.
    write_exec(
        &dir.join("rig-mode.sh"),
        "#!/usr/bin/env bash\nset -euo pipefail\necho \"RIG-MODE-TEST-RESTORE $*\"\nexit 0\n",
    );
    // Fake obs_phase2.py — record start = exit 0; stop = print a fake Windows path (or nothing,
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
    // Fake lipsync_envelope_corr.py — prints corr=<Nth of CORR_SEQ> and increments a counter file,
    // so successive ATTEMPTS get successive scripted correlations (clamped to the last value).
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
    // Fake sshpass on PATH: for scp (win_ssh_download) touch the LOCAL dest (last arg) so the probe
    // file exists; for ssh (cam2 cam_ssh / win_ssh_run) just succeed. Errors suppressed (a remote
    // dest on the upload scp is not touchable — harmless, the flow only needs exit 0).
    write_exec(
        &dir.join("bin/sshpass"),
        "#!/usr/bin/env bash\n\
         if [ \"${3:-}\" = \"scp\" ]; then touch \"${!#}\" 2>/dev/null || true; fi\n\
         exit 0\n",
    );
    fs::write(dir.join("asset.mp4"), b"dummy").unwrap();
}

/// Run `lipsync-test-mode.sh start` in the hermetic rig with a scripted corr sequence. `tmp` is
/// held for the whole `output()` (which blocks until bash exits, so the dir is live throughout),
/// then dropped at return — no dangling handle.
fn run_arrival(corr_seq: &str, retries: &str, stop_empty: bool) -> Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let dir = tmp.path().to_path_buf();
    setup(&dir);
    let path_env = format!(
        "{}:{}",
        dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(dir.join("lipsync-test-mode.sh"))
        .arg("start")
        .arg(dir.join("asset.mp4"))
        .env("PATH", path_env)
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

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// (1) corr >= threshold on the first attempt -> ACTIVE, exit 0, TEST mode NOT restored.
#[test]
fn arrival_passes_on_first_attempt_reaches_active_1192() {
    let out = run_arrival("0.9", "4", false);
    let all = combined(&out);
    assert!(out.status.success(), "arrival pass must exit 0: {all}");
    assert!(
        all.contains("arrival verify PASSED"),
        "must report the arrival verify passed: {all}"
    );
    assert!(
        all.contains("RESULT: lipsync-test mode ACTIVE"),
        "a passing arrival must reach the ACTIVE message: {all}"
    );
    assert!(
        !all.contains("RIG-MODE-TEST-RESTORE"),
        "a passing arrival must NOT restore TEST mode (no ERR trap fired): {all}"
    );
}

/// (2) low corr then high -> recycle mpv once, then ACTIVE, exit 0.
#[test]
fn arrival_recycles_mpv_on_low_corr_then_passes_1192() {
    let out = run_arrival("0.3 0.9", "4", false);
    let all = combined(&out);
    assert!(out.status.success(), "retry-then-pass must exit 0: {all}");
    assert!(
        all.contains("arrival attempt 1/4") && all.contains("arrival attempt 2/4"),
        "must log both attempts with their corr: {all}"
    );
    assert_eq!(
        all.matches("recycling mpv playback").count(),
        1,
        "exactly one recycle between the low attempt and the passing one: {all}"
    );
    assert!(
        all.contains("arrival verify PASSED") && all.contains("RESULT: lipsync-test mode ACTIVE"),
        "the second attempt must pass and reach ACTIVE: {all}"
    );
    assert!(
        !all.contains("RIG-MODE-TEST-RESTORE"),
        "a run that eventually passes must NOT restore TEST mode: {all}"
    );
}

/// (3) all retries below threshold -> FAIL LOUD with the attempt matrix, TEST mode restored via the
/// ERR trap, exit nonzero, no ACTIVE. This is the #1133 blind-spot the static anchors cannot cover.
#[test]
fn arrival_exhaustion_fails_loud_and_restores_test_mode_1192() {
    let out = run_arrival("0.3 0.3", "2", false);
    let all = combined(&out);
    assert!(
        !out.status.success(),
        "exhaustion must exit nonzero (fail loud), not silently proceed: {all}"
    );
    assert!(
        all.contains("asset speech never reached mbc after 2 attempts"),
        "exhaustion must fail loud naming the arrival failure: {all}"
    );
    assert!(
        all.contains("Attempt matrix:") && all.contains("attempt 2/2: envelope corr=0.3"),
        "exhaustion must print the per-attempt matrix: {all}"
    );
    assert!(
        all.contains("RIG-MODE-TEST-RESTORE"),
        "exhaustion must restore TEST mode via the ERR trap (rig-mode.sh test): {all}"
    );
    assert!(
        !all.contains("RESULT: lipsync-test mode ACTIVE"),
        "exhaustion must NEVER print a silent ACTIVE: {all}"
    );
}

/// (4) an infra failure (StopRecord returns no path) -> FAIL LOUD, TEST mode restored, exit nonzero
/// -- proving a genuine infra error inside the window also fires the ERR-trap restore.
#[test]
fn arrival_infra_stoprecord_empty_fails_loud_and_restores_1192() {
    let out = run_arrival("0.9", "4", true);
    let all = combined(&out);
    assert!(
        !out.status.success(),
        "an infra failure must exit nonzero: {all}"
    );
    assert!(
        all.contains("StopRecord returned no path"),
        "must fail loud on the empty StopRecord path: {all}"
    );
    assert!(
        all.contains("RIG-MODE-TEST-RESTORE"),
        "an infra failure inside the window must restore TEST mode via the ERR trap: {all}"
    );
    assert!(
        !all.contains("RESULT: lipsync-test mode ACTIVE"),
        "an infra failure must never reach ACTIVE: {all}"
    );
}
