//! #403 — cam-disk-guard observation-log race (sibling of the #394 rig-restore-watchdog fix).
//!
//! Under the systemd `cam-disk-guard.service` (Type=oneshot), `obs="$(probe_cam …)"` forks a
//! short-lived subshell per cam; `log()` stderr lines from those fast-exiting children are
//! intermittently LOST by journald — the identical race #394 fixed in the watchdog. The probes must
//! run in the long-lived MAIN shell with their records redirected to a file (a redirection does not
//! fork), so the journal reliably gets the observation log. These lock the structural fix (the
//! journald drop itself is environmental/timing, not deterministically unit-testable): no `$()`
//! around the probes, a `collect_observations` helper, an executed-not-sourced guard, and
//! byte-identical `DISK_OBS` collected in the main process.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cam_disk_guard() -> PathBuf {
    manifest_dir().join("scripts/cam-disk-guard.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cam-disk-guard-{}-{}", std::process::id(), name));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_bash(script: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn cam_disk_guard_does_not_collect_observations_via_command_substitution() {
    let src = fs::read_to_string(cam_disk_guard()).expect("read cam-disk-guard");
    assert!(
        !src.contains("$(probe_cam"),
        "#403: probes must NOT run inside $() command substitutions (short-lived subshells whose \
         journald log lines are intermittently lost under the systemd unit)"
    );
    assert!(
        src.contains("collect_observations"),
        "#403: cam-disk-guard must collect observations via a collect_observations helper \
         (main-shell collection, mirroring the #394 watchdog fix)"
    );
}

#[test]
fn cam_disk_guard_runs_main_only_when_executed_not_sourced() {
    // Sourcing the script (tests) must only define functions/config — never run a live pass. This
    // is what makes collect_observations unit-testable with stubbed probes.
    let src = fs::read_to_string(cam_disk_guard()).expect("read cam-disk-guard");
    assert!(
        src.contains(r#"if [[ "${BASH_SOURCE[0]}" == "$0" ]]"#),
        "#403: main must be gated behind an executed-not-sourced guard"
    );
}

#[test]
fn collect_observations_runs_probes_in_the_main_shell_and_preserves_records() {
    // Functional lock: source the guard (guard skips main), stub probe_cam to emit its $BASHPID,
    // and verify (a) DISK_OBS carries all 3 cam records byte-identically in order, and (b) every
    // probe ran in the MAIN shell (pid == the main shell's BASHPID — a $() subshell would fork and
    // report a different pid). Static precondition first so a missing helper fails fast.
    let src = fs::read_to_string(cam_disk_guard()).expect("read cam-disk-guard");
    assert!(
        src.contains("collect_observations"),
        "#403: collect_observations helper missing — cannot run the functional lock"
    );
    let dir = scratch("collect-obs");
    // Unroutable TEST-NET hosts + 1s timeout + --dry-run: even a regressed script that runs a real
    // pass on source stays rig-free, harmless, and fast.
    let script = format!(
        r#"set -u
export CAM1_IP=192.0.2.1 CAM2_IP=192.0.2.2 CAM4_IP=192.0.2.4
export CAM_DISK_GUARD_SSH_TIMEOUT=1
. "{g}" --dry-run
main_pid=$BASHPID
probe_cam() {{ printf 'cam=%s mount=/ used_pct=0 pid=%s\n' "$1" "$BASHPID"; }}
collect_observations
printf 'MAIN=%s\n' "$main_pid"
printf 'BEGIN\n%s\nEND\n' "$DISK_OBS"
"#,
        g = cam_disk_guard().display()
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "#403: source + collect must succeed\nstdout:{stdout}\nstderr:{stderr}"
    );
    let main_pid = stdout
        .lines()
        .find_map(|l| l.strip_prefix("MAIN="))
        .expect("MAIN= line")
        .trim()
        .to_string();
    let body: Vec<&str> = stdout
        .lines()
        .skip_while(|l| *l != "BEGIN")
        .skip(1)
        .take_while(|l| *l != "END")
        .collect();
    let expected: Vec<String> = vec![
        format!("cam=cam1 mount=/ used_pct=0 pid={main_pid}"),
        format!("cam=cam2 mount=/ used_pct=0 pid={main_pid}"),
        format!("cam=cam4 mount=/ used_pct=0 pid={main_pid}"),
    ];
    assert_eq!(
        body,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "#403: DISK_OBS must carry the 3 cam records byte-identically AND every probe must run in \
         the MAIN shell (pid == main BASHPID; a $() subshell would fork)\nstderr:{stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}
