//! issue 1317 part 6 — the strih-lx EXECUTE arm of `scripts/deploy-genlock-fleet.sh`.
//!
//! Before it, execute mode dropped strih-lx (the production strih since the M4 cut-over) and the
//! supervisor deployed it with an ad-hoc scratch script that twice failed half-silently: the box's
//! `/tmp` quota was full, rsync died with `Disk quota exceeded` (rc 11), and the old build simply
//! kept running with nothing refusing. The arm (in `scripts/lib/strih-lx-deploy.sh`) now:
//!
//! * resolves the strih FULL artifact from the linux-genlock run at the anchor's SAME SHA and refuses
//!   an artifact whose own `GENLOCK_BUILD_SHA.txt` is another commit;
//! * sweeps the stale `/tmp/genlock-stage-*` dirs FIRST through the existing
//!   `obs-backup-retention.sh --local-sweep` decision, with the stage being deployed touched newest
//!   so the sweep can never delete it;
//! * stages the whole tree while the old OBS keeps running — an rsync failure exits 4 naming the
//!   step BEFORE anything is stopped;
//! * stops OBS only through the sanctioned stop code (`strih-obs-stop.sh`), never `kill -9`;
//! * runs `setup-strih.sh` as root with the GH token on STDIN only (never an argv, never a file);
//! * reads back and REFUSES (exit 4) unless the installed marker == the canonical SHA,
//!   `strih-obs.service` is active, and `:8899` reports that SHA.
//!
//! These tests drive the real script with `gh`/`sshpass`/`ssh`/`rsync`/`curl` stubbed on PATH, so
//! no rig and no network is touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHA: &str = "abc123def4567890";
const OTHER: &str = "000000000000bad0";
const TOKEN: &str = "tok-SECRET-123";

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
/// launch is captured to $STUB_DIR/sweep.stdin / setup.stdin.
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
    if [ -n "${STUB_NO_LINUX_RUN:-}" ]; then echo '[]'
    else printf '[{"databaseId":777,"headSha":"%s","conclusion":"success"}]\n' "$STUB_SHA"; fi ;;
  "run download")
    d=""; while [ "$#" -gt 0 ]; do [ "$1" = "-D" ] && d="$2"; shift; done
    mkdir -p "$d/bin" "$d/lib/x86_64-linux-gnu/obs-plugins"
    : > "$d/bin/obs"
    echo "${STUB_ARTIFACT_SHA:-$STUB_SHA}" > "$d/GENLOCK_BUILD_SHA.txt" ;;
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
  *--local-sweep*) cat > "$STUB_DIR/sweep.stdin"; echo "SWEEP"; exit "${STUB_SWEEP_RC:-0}" ;;
  *setup-strih.rc*) [ -n "${STUB_SETUP_NO_RC:-}" ] || echo "${STUB_SETUP_RC:-0}"; exit 0 ;;
  *run-setup.sh*) cat > "$STUB_DIR/setup.stdin"; exit "${STUB_LAUNCH_RC:-0}" ;;
  *setup-strih.log*) echo "setup log tail"; exit 0 ;;
  *strih-obs-stop.sh*) exit "${STUB_STOP_RC:-0}" ;;
  *GENLOCK_BUILD_SHA.txt*) printf 'installed=%s active=%s\n' "${STUB_INSTALLED:-$STUB_SHA}" "${STUB_ACTIVE:-active}"; exit 0 ;;
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
}

fn run_exec(extra_env: &[(&str, &str)]) -> Run {
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
    cmd.args(["--run-id", "R1", "--boxes", "strih-lx"])
        .current_dir(manifest_dir())
        .env("PATH", path)
        .env("HOME", &home)
        .env("STUB_DIR", dir.path())
        .env("STUB_SHA", SHA)
        .env("STUB_TOKEN", TOKEN)
        .env("STRIH_LX_SETUP_POLLS", "3")
        .env("STRIH_LX_SETUP_POLL_SECS", "0")
        .env("STRIH_LX_VERIFY_POLLS", "2")
        .env("STRIH_LX_VERIFY_POLL_SECS", "0")
        .env_remove("STRIH_LX_IP");
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

/// The happy path: every step in order, the stage fully staged BEFORE the graceful stop, the token
/// only on the setup launch's stdin, a fail-closed read-back that passes, and the durable fleet log
/// now records strih-lx (it is really deployed).
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
    // order: prep(touch) < sweep < rsync bundle < rsync repo < stop < launch < rc poll < start < readback < :8899
    let prep = r.at(&format!("touch '{stage}'"));
    let sweep = r.at("--local-sweep");
    let rs_bundle = r.at(&format!("{stage}/bundle/"));
    let rs_repo = r.at(&format!("{stage}/repo/"));
    let stop = r.at("strih-obs-stop.sh");
    let launch = r.at("run-setup.sh");
    let rc = r.at("setup-strih.rc");
    let start = r.at("--user start");
    let readback = r.at("GENLOCK_BUILD_SHA.txt");
    let bs = r.at("curl ");
    assert!(
        prep < sweep && sweep < rs_bundle && rs_bundle < stop && rs_repo < stop,
        "the stage is complete BEFORE the stop:\n{}",
        r.calls
    );
    assert!(
        stop < launch && launch < rc && rc < start && start < readback && readback < bs,
        "stop -> setup -> start -> read-back:\n{}",
        r.calls
    );
    // the sweep reuses obs-backup-retention.sh's own --local-sweep decision (password line, then
    // the script itself on stdin), keeps only the newest stage (the just-touched one).
    assert!(
        r.calls
            .contains("--stage-parent /tmp --keep-runs 1 --keep-days 0 --execute"),
        "{}",
        r.calls
    );
    let sweep_stdin = r.file("sweep.stdin");
    assert!(
        sweep_stdin.starts_with("newlevel\n") && sweep_stdin.contains("obs_backup_sweep()"),
        "the sweep program is obs-backup-retention.sh fed after the sudo password line:\n{sweep_stdin}"
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
    // never a hard kill from the deploy.
    assert!(
        !r.calls.contains("kill -9")
            && !r.calls.contains("pkill -KILL")
            && !r.calls.contains("-KILL"),
        "the deploy never force-kills OBS:\n{}",
        r.calls
    );
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

/// A failed sweep (the quota cannot be freed) is loud too, before any byte is staged.
#[test]
fn strih_lx_sweep_failure_is_loud_before_staging_1317() {
    let r = run_exec(&[("STUB_SWEEP_RC", "1")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(r.err.contains("[strih-lx sweep]"), "{}", r.err);
    assert!(
        !r.calls.contains("rsync "),
        "no staging after a failed sweep:\n{}",
        r.calls
    );
}

/// The SHA read-back is a REFUSAL, not a printout: installed marker, unit state and :8899 must
/// all agree with the canonical SHA.
#[test]
fn strih_lx_readback_refuses_a_wrong_sha_or_dead_unit_1317() {
    for (env, want) in [
        (("STUB_INSTALLED", OTHER), "GENLOCK_BUILD_SHA.txt"),
        (("STUB_ACTIVE", "failed"), "strih-obs.service"),
        (("STUB_BS_SHA", OTHER), ":8899"),
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
/// up best-effort so the box is not left dark — but the deploy is FAILED (exit 4, no log line).
#[test]
fn strih_lx_setup_failure_is_loud_and_restarts_obs_best_effort_1317() {
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
    // a setup that never writes its rc within the poll budget is a named timeout, not a hang.
    let r = run_exec(&[("STUB_SETUP_NO_RC", "1")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx setup]") && r.err.contains("no rc"),
        "{}",
        r.err
    );
}

/// A graceful stop that does not complete is a refusal, never an escalation to a hard kill.
#[test]
fn strih_lx_stop_failure_refuses_without_a_hard_kill_1317() {
    let r = run_exec(&[("STUB_STOP_RC", "5")]);
    assert_eq!(r.code, 4, "err={}", r.err);
    assert!(r.err.contains("[strih-lx stop]"), "{}", r.err);
    assert!(
        !r.calls.contains("run-setup.sh"),
        "no install over a still-running OBS:\n{}",
        r.calls
    );
}

/// The same-SHA contract: an artifact whose own marker is another commit, or no linux-genlock run
/// at the SHA, is a resolution failure (exit 3) before the box is touched.
#[test]
fn strih_lx_resolution_refuses_a_foreign_artifact_or_missing_run_1317() {
    let r = run_exec(&[("STUB_ARTIFACT_SHA", OTHER)]);
    assert_eq!(r.code, 3, "err={}", r.err);
    assert!(
        r.err.contains("[strih-lx download]") && r.err.contains(OTHER),
        "{}",
        r.err
    );
    assert!(
        !r.calls.contains("ssh "),
        "the box is never touched:\n{}",
        r.calls
    );

    let r = run_exec(&[("STUB_NO_LINUX_RUN", "1")]);
    assert_eq!(r.code, 3, "err={}", r.err);
    assert!(r.err.contains("[strih-lx resolve]"), "{}", r.err);
    assert!(
        !r.calls.contains("ssh "),
        "the box is never touched:\n{}",
        r.calls
    );
}

/// The pure verdict the read-back uses.
#[test]
fn strih_lx_deploy_verdict_is_pure_and_fail_closed_1317() {
    let run = |args: &str| {
        let o = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -uo pipefail\n. \"$LIB\"\nstrih_lx_deploy_verdict {args}; echo \"rc=$?\""
            ))
            .env("LIB", lib())
            .output()
            .expect("bash");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let ok = run(&format!("{SHA} {SHA} active {SHA}"));
    assert!(ok.contains("OK") && ok.contains("rc=0"), "{ok}");
    for args in [
        format!("{SHA} {OTHER} active {SHA}"),
        format!("{SHA} '' active {SHA}"),
        format!("{SHA} {SHA} activating {SHA}"),
        format!("{SHA} {SHA} active ''"),
        format!("{SHA} {SHA} active {OTHER}"),
        format!("'' '' active ''"),
    ] {
        let o = run(&args);
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
