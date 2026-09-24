//! issue 1317 part 6 — the strih-lx EXECUTE arm of `scripts/deploy-genlock-fleet.sh`.
//!
//! Before it, execute mode dropped strih-lx (the production strih since the M4 cut-over) and the
//! supervisor deployed it with an ad-hoc scratch script that twice failed half-silently: the box's
//! `/tmp` quota was full, rsync died with `Disk quota exceeded` (rc 11), and the old build simply
//! kept running with nothing refusing. The arm (in `scripts/lib/strih-lx-deploy.sh`) now:
//!
//! * resolves AND downloads every requested box's same-SHA artifact before any box is changed, and
//!   refuses a strih artifact whose own `GENLOCK_BUILD_SHA.txt` is another commit;
//! * refuses to start while a previous `setup-strih.sh` is still running on the box;
//! * sweeps the stale `/tmp/genlock-stage-*` dirs FIRST through the existing
//!   `obs-backup-retention.sh --local-sweep` decision (stage dirs only, as the operator, never sudo),
//!   with the stage being deployed touched newest so the sweep can never delete it;
//! * stages the whole tree while the old OBS keeps running — an rsync failure exits 4 naming the
//!   step BEFORE anything is stopped;
//! * stops OBS only through the sanctioned stop code (`strih-obs-stop.sh`); the deploy itself never
//!   sends a kill;
//! * runs `setup-strih.sh` as root with the GH token on STDIN only (never an argv, never a file);
//! * reads back and REFUSES (exit 4) unless the installed marker == the canonical SHA, the installed
//!   libobs bytes match the bundle manifest, `strih-obs.service` is active with the SAME MainPID and
//!   restart count on two consecutive polls, the new OBS log shows `render tick ENABLED`, and `:8899`
//!   reports the canonical SHA.
//!
//! These tests drive the real script with `gh`/`sshpass`/`ssh`/`rsync`/`curl` stubbed on PATH, so
//! no rig and no network is touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const SHA: &str = "abc123def4567890";
const OTHER: &str = "000000000000bad0";
const TOKEN: &str = "tok-SECRET-123";
const LIBSHA: &str = "libobs-bytes-good";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/deploy-genlock-fleet.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/strih-lx-deploy.sh")
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A stub bin dir: every external command the execute arm uses, configured by STUB_* env vars and
/// logging each call (argv only) to $STUB_DIR/calls.log. stdin handed to the sweep / the setup
/// launch is captured to $STUB_DIR/sweep.stdin / setup.stdin. The ssh stub dispatches on the remote
/// command text, so the step commands keep distinguishing substrings.
fn stub_bin(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("gh"),
        r#"#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "$STUB_DIR/calls.log"
case "$1 $2" in
  "run view") echo "$STUB_SHA" ;;
  "run list")
    case "$*" in
      *windows-genlock*) [ -n "${STUB_NO_WIN_RUN:-}" ] && { echo '[]'; exit 0; } ;;
      *linux-genlock*) [ -n "${STUB_NO_LINUX_RUN:-}" ] && { echo '[]'; exit 0; } ;;
    esac
    printf '[{"databaseId":777,"headSha":"%s","conclusion":"success"}]\n' "$STUB_SHA" ;;
  "run download")
    d=""; while [ "$#" -gt 0 ]; do [ "$1" = "-D" ] && d="$2"; shift; done
    mkdir -p "$d/bin" "$d/lib/x86_64-linux-gnu/obs-plugins"
    : > "$d/bin/obs"
    echo "${STUB_ARTIFACT_SHA:-$STUB_SHA}" > "$d/GENLOCK_BUILD_SHA.txt"
    if [ -n "${STUB_MANIFEST_NO_LIB:-}" ]; then
      printf '{"files":[{"path":"bin/obs","sha256":"x"}]}\n' > "$d/BUNDLE_MANIFEST.json"
    else
      printf '{"files":[{"path":"lib/x86_64-linux-gnu/libobs.so.30","sha256":"%s"}]}\n' "$STUB_LIBSHA" > "$d/BUNDLE_MANIFEST.json"
    fi ;;
  "auth token") echo "$STUB_TOKEN" ;;
  *) echo "gh stub: unhandled $*" >&2; exit 9 ;;
esac
"#,
    );
    write_exec(
        &bin.join("sshpass"),
        r#"#!/usr/bin/env bash
printf 'sshpass %s\n' "$*" >> "$STUB_DIR/calls.log"
[ "$1" = "-p" ] && shift 2
exec "$@"
"#,
    );
    write_exec(
        &bin.join("ssh"),
        r#"#!/usr/bin/env bash
printf 'ssh %s\n' "$*" >> "$STUB_DIR/calls.log"
cmd="${@: -1}"
case "$cmd" in
  *"pgrep -x setup-strih.sh"*)
    if [ -f "$STUB_DIR/setup.stdin" ]; then st="${STUB_INSTALLER_AFTER:-idle}"; else st="${STUB_INSTALLER:-idle}"; fi
    [ "$st" = unreachable ] && exit 255
    echo "$st" ;;
  *--local-sweep*) cat > "$STUB_DIR/sweep.stdin"; echo "SWEEP"; exit "${STUB_SWEEP_RC:-0}" ;;
  *LEFTOVER*) [ -n "${STUB_LEFTOVER:-}" ] && echo "LEFTOVER $STUB_LEFTOVER"; exit "${STUB_STAGECHECK_RC:-0}" ;;
  *setup-strih.rc*) [ -n "${STUB_SETUP_NO_RC:-}" ] || echo "${STUB_SETUP_RC:-0}"; exit 0 ;;
  *run-setup.sh*) cat > "$STUB_DIR/setup.stdin"; exit "${STUB_LAUNCH_RC:-0}" ;;
  *"reboot strih-lx, then run verify-strih"*) [ -n "${STUB_REBOOT_LINE:-}" ] && echo "$STUB_REBOOT_LINE"; exit 0 ;;
  *setup-strih.log*) echo "setup log tail"; exit 0 ;;
  *strih-obs-stop.sh*) exit "${STUB_STOP_RC:-0}" ;;
  *GENLOCK_BUILD_SHA.txt*)
    n=0; [ -f "$STUB_DIR/readback.n" ] && n="$(cat "$STUB_DIR/readback.n")"; n=$((n + 1)); echo "$n" > "$STUB_DIR/readback.n"
    pid=4242; restarts=0
    if [ -n "${STUB_CRASHLOOP:-}" ]; then pid=$((4242 + n)); restarts="$n"; fi
    if [ -n "${STUB_LATE_CRASHLOOP:-}" ] && [ "$n" -ge 3 ]; then pid=$((4242 + n)); restarts="$n"; fi
    printf 'installed=%s active=%s pid=%s restarts=%s lib=%s tick=%s\n' "${STUB_INSTALLED:-$STUB_SHA}" "${STUB_ACTIVE:-active}" "$pid" "$restarts" "${STUB_LIB:-$STUB_LIBSHA}" "${STUB_TICK:-1}"; exit 0 ;;
  *"--user start"*) exit "${STUB_START_RC:-0}" ;;
  *) exit "${STUB_SSH_RC:-0}" ;;
esac
"#,
    );
    write_exec(
        &bin.join("rsync"),
        r#"#!/usr/bin/env bash
printf 'rsync %s\n' "$*" >> "$STUB_DIR/calls.log"
if [ -n "${STUB_RSYNC_FAIL_ON:-}" ]; then
  case "$*" in *"$STUB_RSYNC_FAIL_ON"*) echo "rsync: write failed: Disk quota exceeded (122)" >&2; exit 11 ;; esac
fi
exit 0
"#,
    );
    write_exec(
        &bin.join("curl"),
        r#"#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$STUB_DIR/calls.log"
printf '{"genlock_build_sha":"%s"}\n' "${STUB_BS_SHA:-$STUB_SHA}"
"#,
    );
    bin
}

struct Run {
    code: i32,
    out: String,
    err: String,
    calls: String,
    dir: tempfile::TempDir,
}

impl Run {
    fn file(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }
    fn fleet_log(&self) -> String {
        fs::read_to_string(
            self.dir
                .path()
                .join("home/.camera-box/genlock-fleet-deploy.log"),
        )
        .unwrap_or_default()
    }
    /// index of the first calls.log line containing `needle` (panics when absent).
    fn at(&self, needle: &str) -> usize {
        self.calls
            .lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no call containing `{needle}`:\n{}", self.calls))
    }
    /// index of the LAST calls.log line containing `needle`, or None.
    fn last(&self, needle: &str) -> Option<usize> {
        self.calls
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains(needle))
            .map(|(i, _)| i)
            .last()
    }
}

fn run_exec_boxes(boxes: &str, extra_env: &[(&str, &str)]) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = stub_bin(dir.path());
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new(script());
    cmd.args(["--run-id", "R1", "--boxes", boxes])
        .current_dir(manifest_dir())
        .env("PATH", path)
        .env("HOME", &home)
        .env("STUB_DIR", dir.path())
        .env("STUB_SHA", SHA)
        .env("STUB_TOKEN", TOKEN)
        .env("STUB_LIBSHA", LIBSHA)
        .env("STRIH_LX_SETUP_POLLS", "3")
        .env("STRIH_LX_SETUP_POLL_SECS", "0")
        .env("STRIH_LX_VERIFY_POLLS", "3")
        .env("STRIH_LX_VERIFY_POLL_SECS", "0")
        .env("STRIH_LX_VERIFY_SETTLE_SECS", "0")
        .env_remove("STRIH_LX_IP")
        .env_remove("STRIH_LX_USER")
        .env_remove("STRIH_LX_PW")
        .env_remove("STRIH_LX_GH_TOKEN");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let o = cmd.output().expect("run deploy-genlock-fleet.sh");
    let calls = fs::read_to_string(dir.path().join("calls.log")).unwrap_or_default();
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
        calls,
        dir,
    }
}

fn run_exec(extra_env: &[(&str, &str)]) -> Run {
    run_exec_boxes("strih-lx", extra_env)
}

/// The happy path: every step in order, the stage fully staged BEFORE the graceful stop, the token
/// only on the setup launch's stdin, a fail-closed read-back that passes on two stable polls, and
/// the durable fleet log now records strih-lx (it is really deployed).
#[test]
fn strih_lx_execute_stages_first_then_stops_sets_up_starts_and_verifies_1317() {
    let r = run_exec(&[]);
    assert_eq!(
        r.code, 0,
        "a clean strih-lx execute must succeed.\nout={}\nerr={}\ncalls={}",
        r.out, r.err, r.calls
    );
    let stage = format!("/tmp/genlock-stage-{SHA}");
    // same-SHA resolution: the linux-genlock run at the anchor SHA, the strih FULL artifact.
    assert!(
        r.calls.contains("--workflow linux-genlock.yml"),
        "{}",
        r.calls
    );
    assert!(
        r.calls.contains(
            "run download 777 --repo zbynekdrlik/camera-box -n obs-genlock-linux-x86_64-strih"
        ),
        "downloads the strih FULL artifact from the same-SHA run:\n{}",
        r.calls
    );
    // order: installer preflight < prep(touch) < sweep < stage check < rsync bundle/repo < stop
    //        < launch < rc poll < start < read-back < :8899
    let pre = r.at("pgrep -x setup-strih.sh");
    let prep = r.at(&format!("touch '{stage}'"));
    let sweep = r.at("--local-sweep");
    let check = r.at("LEFTOVER");
    let rs_bundle = r.at(&format!("{stage}/bundle/"));
    let rs_repo = r.at(&format!("{stage}/repo/"));
    let stop = r.at("strih-obs-stop.sh");
    let launch = r.at("run-setup.sh");
    let rc = r.at("setup-strih.rc");
    let start = r.at("--user start");
    let readback = r.at("GENLOCK_BUILD_SHA.txt");
    let bs = r.at("curl ");
    assert!(
        pre < prep
            && prep < sweep
            && sweep < check
            && check < rs_bundle
            && rs_bundle < stop
            && rs_repo < stop,
        "the stage is complete BEFORE the stop:\n{}",
        r.calls
    );
    assert!(
        stop < launch && launch < rc && rc < start && start < readback && readback < bs,
        "stop -> setup -> start -> read-back:\n{}",
        r.calls
    );
    // the read-back needs TWO consecutive good polls (MainPID + NRestarts stable).
    assert!(
        r.calls.matches("GENLOCK_BUILD_SHA.txt").count() >= 2,
        "a single momentary `active` is never enough:\n{}",
        r.calls
    );
    // the sweep reuses obs-backup-retention.sh's own --local-sweep decision, stage dirs only, as
    // the operator (no sudo, no password line), from the COMMITTED tree.
    let sweep_line = r.calls.lines().nth(sweep).unwrap();
    assert!(
        sweep_line.contains("--stages-only")
            && sweep_line.contains("--stage-parent /tmp --keep-runs 1 --keep-days 0 --execute")
            && !sweep_line.contains("sudo"),
        "{sweep_line}"
    );
    let sweep_stdin = r.file("sweep.stdin");
    assert!(
        !sweep_stdin.starts_with("newlevel") && sweep_stdin.contains("obs_backup_sweep()"),
        "the sweep program is obs-backup-retention.sh itself, no password line:\n{sweep_stdin}"
    );
    // the GH token travels ONLY on the setup launch's stdin.
    assert!(
        r.file("setup.stdin").contains(&format!("ghtoken:{TOKEN}")),
        "the setup launch receives the token on stdin"
    );
    assert!(
        !r.calls.contains(TOKEN) && !r.out.contains(TOKEN) && !r.err.contains(TOKEN),
        "the GH token never appears in any argv or output:\n{}",
        r.calls
    );
    // the deploy itself sends no kill (the stop is delegated to strih-obs-stop.sh).
    for k in ["kill -9", "-KILL", "SIGKILL", "killall", "kill -s"] {
        assert!(
            !r.calls.contains(k),
            "the deploy never sends `{k}`:\n{}",
            r.calls
        );
    }
    // every remote command is time-bounded.
    for l in r.calls.lines().filter(|l| l.starts_with("sshpass ")) {
        if l.contains(" rsync ") {
            assert!(
                l.contains("--timeout="),
                "rsync without an I/O timeout: {l}"
            );
        } else if l.contains(" ssh ") {
            assert!(l.contains(" timeout "), "unbounded ssh call: {l}");
        }
    }
    assert!(
        r.fleet_log().contains(SHA) && r.fleet_log().contains("strih-lx"),
        "the durable fleet log records the deployed strih-lx:\n{}",
        r.fleet_log()
    );
}

/// The 23.9. incident: rsync dies on the full /tmp quota. The deploy must exit 4 NAMING the step,
/// and must not have stopped OBS (the old build keeps running, on purpose and visibly).
#[test]
fn strih_lx_rsync_failure_is_loud_and_never_stops_obs_1317() {
    let r = run_exec(&[("STUB_RSYNC_FAIL_ON", "/bundle/")]);
    assert_eq!(r.code, 4, "rsync failure = exit 4.\nerr={}", r.err);
    assert!(
        r.err.contains("[strih-lx stage]") && r.err.contains("rc=11"),
        "the error names the step and the rsync rc:\n{}",
        r.err
    );
    assert!(
        !r.calls.contains("strih-obs-stop.sh") && !r.calls.contains("run-setup.sh"),
        "nothing is stopped or installed after a failed stage:\n{}",
        r.calls
    );
    assert!(
        r.fleet_log().is_empty(),
        "no fleet-log line for a failed deploy"
    );
}

/// A failed sweep (the quota cannot be freed), a failed stage prep, or a stage that vanished are
/// loud too, before any byte is staged. A stage dir the sweep could not remove is a loud WARNING.
#[test]
fn strih_lx_sweep_failures_are_loud_before_staging_1317() {
    for env in [
        ("STUB_SWEEP_RC", "1"),
        ("STUB_SSH_RC", "1"),
        ("STUB_STAGECHECK_RC", "3"),
    ] {
        let r = run_exec(&[env]);
        assert_eq!(r.code, 4, "{env:?}: err={}", r.err);
        assert!(r.err.contains("[strih-lx sweep]"), "{env:?}: {}", r.err);
        assert!(
            !r.calls.contains("rsync "),
            "{env:?}: no staging after a failed sweep:\n{}",
            r.calls
        );
    }
    let r = run_exec(&[("STUB_LEFTOVER", "/tmp/genlock-stage-0ld")]);
    assert_eq!(r.code, 0, "a leftover only warns.\nerr={}", r.err);
    assert!(
        r.err.contains("WARNING") && r.err.contains("/tmp/genlock-stage-0ld"),
        "{}",
        r.err
    );
}

/// A previous setup-strih.sh still running on the box: refuse before the sweep could delete the
/// stage it is reading, and never start a second concurrent installer.
#[test]
fn strih_lx_refuses_while_a_previous_installer_runs_1317() {
    let r = run_exec(&[("STUB_INSTALLER", "alive")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx preflight]") && r.err.contains("setup-strih.sh"),
        "{}",
        r.err
    );
    for c in [
        "--local-sweep",
        "rsync ",
        "strih-obs-stop.sh",
        "run-setup.sh",
    ] {
        assert!(!r.calls.contains(c), "`{c}` must not run:\n{}", r.calls);
    }
}

/// The read-back is a REFUSAL, not a printout: marker, installed libobs bytes, unit state, a stable
/// MainPID/NRestarts across two polls (a crash-looping Type=simple unit reads `active` between
/// restarts), the new log's render tick, and :8899 must all agree.
#[test]
fn strih_lx_readback_refuses_a_wrong_sha_dead_or_crashlooping_obs_1317() {
    for (env, want) in [
        (("STUB_INSTALLED", OTHER), "GENLOCK_BUILD_SHA.txt"),
        (("STUB_ACTIVE", "failed"), "strih-obs.service"),
        (("STUB_BS_SHA", OTHER), ":8899"),
        (("STUB_LIB", "libobs-bytes-stale"), "libobs.so.30"),
        (("STUB_TICK", "0"), "render tick ENABLED"),
        (("STUB_CRASHLOOP", "1"), "MainPID"),
    ] {
        let r = run_exec(&[env]);
        assert_eq!(r.code, 4, "{env:?} must refuse.\nerr={}", r.err);
        assert!(
            r.err.contains("[strih-lx verify]") && r.err.contains(want),
            "{env:?}: the refusal names `{want}`:\n{}",
            r.err
        );
        assert!(r.fleet_log().is_empty(), "{env:?}: no fleet-log line");
    }
}

/// setup-strih.sh failing (non-zero rc file) is loud, shows the log tail, and still brings OBS back
/// up best-effort so the box is not left dark — but the deploy is FAILED (exit 4, no log line). A
/// setup that is still running (no rc in the budget, or a launch whose ssh dropped after the detach)
/// never gets OBS started over it.
#[test]
fn strih_lx_setup_failure_is_loud_and_never_starts_obs_over_a_running_installer_1317() {
    let r = run_exec(&[("STUB_SETUP_RC", "1")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx setup]") && r.err.contains("rc=1"),
        "{}",
        r.err
    );
    assert!(
        r.out.contains("setup log tail"),
        "the log tail is shown:\n{}",
        r.out
    );
    assert!(
        r.at("setup-strih.rc") < r.at("--user start"),
        "OBS is restarted best-effort after the failed setup:\n{}",
        r.calls
    );
    assert!(
        r.fleet_log().is_empty(),
        "no fleet-log line for a failed deploy"
    );

    // no rc within the budget, installer still alive -> named timeout, OBS NOT started.
    let r = run_exec(&[("STUB_SETUP_NO_RC", "1"), ("STUB_INSTALLER_AFTER", "alive")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx setup]") && r.err.contains("no rc"),
        "{}",
        r.err
    );
    assert!(
        !r.calls.contains("--user start"),
        "never start OBS over a running installer:\n{}",
        r.calls
    );

    // review round 2: the launch ssh dropped AFTER the detach while this deploy's own installer
    // runs (the preflight saw none) -> follow it through the rc poll, never leave the strih dark.
    let r = run_exec(&[("STUB_LAUNCH_RC", "255"), ("STUB_INSTALLER_AFTER", "alive")]);
    assert_eq!(r.code, 0, "err={}", r.err);
    assert!(
        r.err.contains("WARNING") && r.err.contains("rc=255"),
        "the dropped launch is reported:\n{}",
        r.err
    );
    assert!(
        r.at("run-setup.sh") < r.at("setup-strih.rc")
            && r.at("setup-strih.rc") < r.at("--user start"),
        "{}",
        r.calls
    );

    // launch failed and no installer is running -> the previous OBS is started best-effort.
    let r = run_exec(&[("STUB_LAUNCH_RC", "1")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(
        r.at("run-setup.sh") < r.last("--user start").expect("best-effort start"),
        "{}",
        r.calls
    );
}

/// A graceful stop that does not complete is a refusal (nothing installed) followed by a
/// best-effort start; the deploy itself never escalates to a kill. A failed start is loud.
#[test]
fn strih_lx_stop_or_start_failure_is_loud_1317() {
    let r = run_exec(&[("STUB_STOP_RC", "5")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(r.err.contains("[strih-lx stop]"), "{}", r.err);
    assert!(
        !r.calls.contains("run-setup.sh"),
        "no install over a still-running OBS:\n{}",
        r.calls
    );
    assert!(
        r.at("strih-obs-stop.sh") < r.at("--user start"),
        "best-effort start after a failed stop:\n{}",
        r.calls
    );

    let r = run_exec(&[("STUB_START_RC", "1")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(r.err.contains("[strih-lx start]"), "{}", r.err);
    assert!(r.fleet_log().is_empty(), "no fleet-log line");
}

/// The same-SHA contract and the local preparation: a foreign artifact, a manifest with no libobs
/// entry, no linux-genlock run, an empty GH token, or a non-IPv4 dial override are all exit 3
/// before the box is touched.
#[test]
fn strih_lx_preparation_failures_never_touch_the_box_1317() {
    for (env, step, want) in [
        (("STUB_ARTIFACT_SHA", OTHER), "[strih-lx download]", OTHER),
        (
            ("STUB_MANIFEST_NO_LIB", "1"),
            "[strih-lx download]",
            "libobs.so.30",
        ),
        (
            ("STUB_NO_LINUX_RUN", "1"),
            "[strih-lx resolve]",
            "linux-genlock",
        ),
        (("STUB_TOKEN", ""), "[strih-lx tree]", "GH_TOKEN"),
        (
            ("STRIH_LX_IP", "strih-lx.lan"),
            "[strih-lx resolve]",
            "IPv4",
        ),
        (("STRIH_LX_IP", "999.1.1.1"), "[strih-lx resolve]", "IPv4"),
        (("STRIH_LX_IP", "10.77.9"), "[strih-lx resolve]", "IPv4"),
        (("STRIH_LX_IP", "..."), "[strih-lx resolve]", "IPv4"),
    ] {
        let r = run_exec(&[env]);
        assert_eq!(r.code, 3, "{env:?}: err={}", r.err);
        assert!(
            r.err.contains(step) && r.err.contains(want),
            "{env:?}: `{step}` + `{want}`:\n{}",
            r.err
        );
        assert!(
            !r.calls.contains("ssh ") && !r.calls.contains("rsync "),
            "{env:?}: the box is never touched:\n{}",
            r.calls
        );
    }
}

/// Every requested box's artifact is resolved BEFORE the production strih is changed: a default
/// `strih-lx,stream` run with no Windows build at the SHA fails without touching strih-lx.
#[test]
fn strih_lx_is_not_changed_when_another_box_cannot_be_resolved_1317() {
    let r = run_exec_boxes("strih-lx,stream", &[("STUB_NO_WIN_RUN", "1")]);
    assert_eq!(r.code, 3, "err={}", r.err);
    assert!(
        !r.calls.contains("ssh ") && !r.calls.contains("rsync "),
        "strih-lx untouched:\n{}",
        r.calls
    );
}

/// A read-only GH token override replaces the operator's full-scope `gh auth token`, and a
/// reboot-pending note from setup-strih.sh is passed on, not swallowed by `VERIFIED`.
#[test]
fn strih_lx_token_override_and_reboot_note_1317() {
    let r = run_exec(&[
        ("STRIH_LX_GH_TOKEN", "ro-token-xyz"),
        (
            "STUB_REBOOT_LINE",
            "the shared OBS-box baseline takes effect at the NEXT boot -- reboot strih-lx",
        ),
    ]);
    assert_eq!(r.code, 0, "err={}", r.err);
    assert!(r.file("setup.stdin").contains("ghtoken:ro-token-xyz"));
    assert!(
        !r.calls.contains("gh auth token"),
        "the override is used, gh auth token is not called:\n{}",
        r.calls
    );
    assert!(
        r.out.contains("NOTE") && r.out.contains("reboot strih-lx"),
        "{}",
        r.out
    );
}

/// The pure verdicts the read-back uses.
#[test]
fn strih_lx_deploy_verdicts_are_pure_and_fail_closed_1317() {
    let run = |call: &str| {
        let o = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -uo pipefail\n. \"$LIB\"\n{call}; echo \"rc=$?\""
            ))
            .env("LIB", lib())
            .output()
            .expect("bash");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let v = |args: &str| run(&format!("strih_lx_deploy_verdict {args}"));
    let ok = v(&format!("{SHA} {SHA} active {SHA} L L 1"));
    assert!(ok.contains("OK") && ok.contains("rc=0"), "{ok}");
    for args in [
        format!("{SHA} {OTHER} active {SHA} L L 1"),
        format!("{SHA} '' active {SHA} L L 1"),
        format!("{SHA} {SHA} activating {SHA} L L 1"),
        format!("{SHA} {SHA} active '' L L 1"),
        format!("{SHA} {SHA} active {OTHER} L L 1"),
        format!("{SHA} {SHA} active {SHA} L M 1"),
        format!("{SHA} {SHA} active {SHA} L '' 1"),
        format!("{SHA} {SHA} active {SHA} '' '' 1"),
        format!("{SHA} {SHA} active {SHA} L L 0"),
        "'' '' active '' L L 1".to_string(),
    ] {
        let o = v(&args);
        assert!(o.contains("FAIL") && o.contains("rc=1"), "{args}: {o}");
    }
    let s = |args: &str| run(&format!("strih_lx_stable_verdict {args}"));
    let ok = s("4242 0 4242 0");
    assert!(ok.contains("OK") && ok.contains("rc=0"), "{ok}");
    for args in ["4242 0 4243 1", "4242 0 4242 1", "0 0 0 0", "'' 0 '' 0"] {
        let o = s(args);
        assert!(o.contains("FAIL") && o.contains("rc=1"), "{args}: {o}");
    }
}

/// The stage dir is the retention allowlist shape (so the sweep can prune it later) and a non-hex
/// SHA is refused rather than turned into an unsweepable name.
#[test]
fn strih_lx_stage_dir_matches_the_retention_allowlist_1317() {
    let o = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". \"$LIB\"; strih_lx_stage_dir {SHA}; echo \"rc=$?\"; strih_lx_stage_dir 'x y'; echo \"rc=$?\""
        ))
        .env("LIB", lib())
        .output()
        .expect("bash");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains(&format!("/tmp/genlock-stage-{SHA}\nrc=0")),
        "{out}"
    );
    assert!(out.trim_end().ends_with("rc=2"), "{out}");
}

/// `obs-backup-retention.sh --local-sweep --stages-only` prunes the stale stage dirs and NEVER the
/// dated rollback backups (the deploy only needs the /tmp quota back).
#[test]
fn retention_stages_only_never_touches_dated_backups_1317() {
    let t = tempfile::tempdir().unwrap();
    let backups = t.path().join("backup");
    let stages = t.path().join("tmp");
    let dated = backups.join("2026-09-01T10-00-00-789");
    let old_stage = stages.join("genlock-stage-0a0a");
    let new_stage = stages.join("genlock-stage-0b0b");
    for d in [&dated, &old_stage, &new_stage] {
        fs::create_dir_all(d).unwrap();
    }
    // make the new stage strictly newer.
    let o = Command::new("touch")
        .args(["-d", "2020-01-01", old_stage.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success());
    let o = Command::new("bash")
        .arg(manifest_dir().join("scripts/obs-backup-retention.sh"))
        .args([
            "--local-sweep",
            "--stages-only",
            "--backup-root",
            backups.to_str().unwrap(),
            "--stage-parent",
            stages.to_str().unwrap(),
            "--keep-runs",
            "1",
            "--keep-days",
            "0",
            "--execute",
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(dated.exists(), "dated backups untouched:\n{out}");
    assert!(new_stage.exists(), "newest stage kept:\n{out}");
    assert!(!old_stage.exists(), "stale stage removed:\n{out}");
    assert!(out.contains("--stages-only"), "{out}");
}

/// The generated on-box runner: it takes the token from the `ghtoken:` line (also when a NOPASSWD
/// sudo left the password line unread), exports it with the bundle dir and IP, runs setup-strih.sh
/// DETACHED and writes its rc. The root check is removed for the test only.
#[test]
fn strih_lx_setup_runner_reads_the_token_from_stdin_and_writes_the_rc_1317() {
    for stdin in ["pw\nghtoken:tok42\n", "ghtoken:tok42\n"] {
        let t = tempfile::tempdir().unwrap();
        let s = t.path().join("stage");
        fs::create_dir_all(s.join("repo/scripts")).unwrap();
        fs::create_dir_all(s.join("bundle")).unwrap();
        write_exec(
            &s.join("repo/scripts/setup-strih.sh"),
            "#!/bin/bash\necho \"B=$STRIH_LX_BUNDLE_SRC IP=$STRIH_LX_IP T=$GH_TOKEN\"\nexit 6\n",
        );
        let gen = Command::new("bash")
            .arg("-c")
            .arg(". \"$LIB\"; strih_lx_setup_runner \"$S\" 10.9.9.9")
            .env("LIB", lib())
            .env("S", &s)
            .output()
            .unwrap();
        assert!(gen.status.success());
        let runner = String::from_utf8_lossy(&gen.stdout).replace(
            "[ \"$(id -u)\" = 0 ] || { echo \"run-setup: must run as root\" >&2; exit 2; }",
            "",
        );
        assert!(
            !runner.contains("must run as root"),
            "the root check line moved -- update this test:\n{runner}"
        );
        write_exec(&s.join("repo/run-setup.sh"), &runner);
        let mut child = Command::new("bash")
            .arg(s.join("repo/run-setup.sh"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin.as_bytes())
                .unwrap();
        }
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{stdin:?}");
        let rc = s.join("setup-strih.rc");
        let t0 = Instant::now();
        while !rc.exists() && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fs::read_to_string(&rc).unwrap().trim(), "6", "{stdin:?}");
        let log = fs::read_to_string(s.join("setup-strih.log")).unwrap();
        assert!(
            log.contains(&format!("B={}/bundle", s.display()))
                && log.contains("IP=10.9.9.9")
                && log.contains("T=tok42"),
            "{stdin:?}: {log}"
        );
        // no token line at all -> refuse.
        let mut child = Command::new("bash")
            .arg(s.join("repo/run-setup.sh"))
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"pw\n").unwrap();
        }
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{stdin:?}");
    }
}

/// review round 2: a crash loop slower than one poll interval (the CEF trap fired every ~60 s) must
/// still be caught -- the MainPID/NRestarts must hold from the first good poll for a settle time,
/// not merely across two adjacent polls.
#[test]
fn strih_lx_readback_holds_stability_for_the_settle_time_1317() {
    let r = run_exec(&[
        ("STUB_LATE_CRASHLOOP", "1"),
        ("STRIH_LX_VERIFY_POLLS", "5"),
        ("STRIH_LX_VERIFY_POLL_SECS", "1"),
        ("STRIH_LX_VERIFY_SETTLE_SECS", "3"),
    ]);
    assert_eq!(
        r.code, 4,
        "two stable polls inside the settle time are not enough.\nerr={}",
        r.err
    );
    assert!(
        r.err.contains("[strih-lx verify]") && r.err.contains("MainPID"),
        "{}",
        r.err
    );
    // a stable OBS still verifies once the settle time has passed.
    let r = run_exec(&[
        ("STRIH_LX_VERIFY_POLLS", "6"),
        ("STRIH_LX_VERIFY_POLL_SECS", "1"),
        ("STRIH_LX_VERIFY_SETTLE_SECS", "2"),
    ]);
    assert_eq!(r.code, 0, "err={}", r.err);
}

/// review round 2: the Windows paste-programs are printed only AFTER strih-lx verified (an operator
/// must not paste stream while the strih apply can still fail), and strih-lx is logged the moment it
/// verified, so a later imag failure never hides the strih deploy that happened.
#[test]
fn windows_programs_follow_the_verified_strih_and_strih_is_logged_at_once_1317() {
    let r = run_exec_boxes("strih-lx,stream", &[]);
    assert_eq!(r.code, 0, "err={}\nout={}", r.err, r.out);
    let verified = r.out.find("VERIFIED").expect("strih-lx VERIFIED line");
    let program = r
        .out
        .find("$ErrorActionPreference = 'Stop'")
        .expect("the stream program");
    assert!(verified < program, "{}", r.out);
    let log = r.fleet_log();
    let strih_line = log
        .lines()
        .find(|l| l.contains("\tstrih-lx\t"))
        .unwrap_or_else(|| panic!("a strih-lx fleet-log line:\n{log}"));
    assert!(strih_line.contains(SHA), "{log}");
    assert!(
        log.lines().any(|l| l.contains("\tstream\t")),
        "the rest of the fleet is logged too:\n{log}"
    );

    // an incomplete imag artifact (the stub ships no distroav.so) fails in imag's PREPARATION, which
    // runs before the strih-lx apply -> the production strih is never touched, nothing is logged.
    let r = run_exec_boxes("strih-lx,imag", &[]);
    assert_eq!(r.code, 3, "err={}", r.err);
    assert!(
        !r.calls.contains("ssh ") && r.fleet_log().is_empty(),
        "imag preparation fails BEFORE strih-lx is applied:\n{}",
        r.calls
    );
}

/// review round 3: the reboot note greps setup-strih.sh's OWN pending-reboot warning, not every
/// "next boot" line the baseline prints earlier in the run (`-m 3` used to cut the real one off).
/// Runs the builder's real grep against a fixture log, and pins the coupling to setup-strih.sh's text.
#[test]
fn strih_lx_reboot_note_selects_only_the_pending_reboot_warning_1317() {
    let t = tempfile::tempdir().unwrap();
    let stage = t.path().join("genlock-stage-abc1");
    fs::create_dir_all(&stage).unwrap();
    let real = "  the shared OBS-box baseline takes effect at the NEXT boot -- reboot strih-lx, then run verify-strih.sh (the run below only reports what is still pending)";
    let mut log = String::new();
    for i in 0..5 {
        log.push_str(&format!("  routine {i}: applies at next boot\n"));
    }
    log.push_str(real);
    log.push('\n');
    fs::write(stage.join("setup-strih.log"), log).unwrap();
    let o = Command::new("bash")
        .arg("-c")
        .arg(". \"$LIB\"; bash -c \"$(strih_lx_remote_setup_notes_cmd \"$S\")\"")
        .env("LIB", lib())
        .env("S", &stage)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        out.trim_end(),
        real,
        "only the pending-reboot warning:\n{out}"
    );
    let setup = fs::read_to_string(manifest_dir().join("scripts/setup-strih.sh")).unwrap();
    assert!(
        setup.contains("reboot strih-lx, then run verify-strih"),
        "the deploy's reboot-note key must stay in setup-strih.sh's warning"
    );
}

/// review round 3: a box that became unreachable after a failed launch fails fast (no 45-min rc
/// poll, no start), and a settle time the poll budget cannot outlast is refused up front.
#[test]
fn strih_lx_unreachable_after_launch_and_settle_budget_1317() {
    let r = run_exec(&[
        ("STUB_LAUNCH_RC", "255"),
        ("STUB_INSTALLER_AFTER", "unreachable"),
    ]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx setup]") && r.err.contains("unreachable"),
        "{}",
        r.err
    );
    assert!(
        !r.calls.contains("setup-strih.rc") && !r.calls.contains("--user start"),
        "{}",
        r.calls
    );

    let r = run_exec(&[
        ("STRIH_LX_VERIFY_POLLS", "3"),
        ("STRIH_LX_VERIFY_POLL_SECS", "10"),
        ("STRIH_LX_VERIFY_SETTLE_SECS", "90"),
    ]);
    assert_eq!(r.code, 3, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx resolve]") && r.err.contains("SETTLE"),
        "{}",
        r.err
    );
    assert!(!r.calls.contains("ssh "), "{}", r.calls);
}
