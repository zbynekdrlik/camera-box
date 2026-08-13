//! #882 — imag-nb's OBS being simply NOT RUNNING must be distinguishable from "the process is up
//! but WebSocket port 4455 isn't listening yet" and from "both are up, the real failure is
//! deeper" (handshake/auth or no-matching-monitor, both already handled inside
//! scripts/obs_phase2.py::open_projectors). Live incident (2026-07-30): OBS died at 08:11:28 and
//! every subsequent E2E preflight failure read "could not open the Multiview/Program projectors
//! — check imag-nb's OBS WebSocket is reachable and DP-0/HDMI-0 are actually connected monitors"
//! — both named connectors are WRONG for this box (it has eDP-1/HDMI-1) and the one true fact
//! (OBS was not running at all) was absent from the message.
//!
//! `scripts/lib/imag-obs-reachability.sh` provides the REMOTE probe (embedded via `$(...)` into
//! an ssh command string, mirroring `scripts/lib/imag-require-remote-tool.sh`'s established
//! pattern) plus a pure local message formatter. These tests exercise the probe DIRECTLY (as a
//! "simulated remote" bash invocation with a stubbed `pgrep` and a real loopback TCP
//! listener/non-listener) — no ssh, no rig.

use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/imag-obs-reachability.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const RECORDING_E2E: &str = "scripts/recording-e2e.sh";

/// A fake `pgrep` stub dir: `found = true` means `pgrep -x obs` exits 0 (obs "running"),
/// `found = false` means it exits 1 (obs "absent") — mirrors fake_bin_dir in the sibling #833
/// harness, but the stub always signals a fixed pass/fail rather than checking a real name.
fn fake_pgrep_dir(found: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("pgrep");
    let body = if found {
        "#!/bin/sh\nexit 0\n"
    } else {
        "#!/bin/sh\nexit 1\n"
    };
    fs::write(&p, body).expect("write pgrep stub");
    let mut perm = fs::metadata(&p).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&p, perm).expect("chmod stub");
    dir
}

/// Run `imag_obs_reachability_probe_cmd <port>` as a REMOTE snippet under a "simulated remote"
/// bash whose PATH is restricted to the fake pgrep stub (never the real PATH, which would let a
/// real `pgrep` on this dev box observe whatever's ACTUALLY running here and prove nothing).
fn run_probe(pgrep_found: bool, port: u16) -> String {
    let bin_dir = fake_pgrep_dir(pgrep_found);
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nPATH=\"$FAKEPATH\" /usr/bin/bash -c \"$(imag_obs_reachability_probe_cmd {port})\""
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("FAKEPATH", bin_dir.path())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "the probe snippet must always exit 0 (the CALLER decides what the line means).\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn message_for(probe_line: &str) -> String {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\nimag_obs_reachability_message \"$PROBE\"";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", lib_script())
        .env("PROBE", probe_line)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "imag_obs_reachability_message must exit 0.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn process_absent_is_reported_first_regardless_of_port_state() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    // Even with something LISTENING on the port, an absent process must win -- the process check
    // runs first and short-circuits (mirrors the real incident: OBS was simply not running).
    let out = run_probe(false, port);
    assert_eq!(out, "OBS_PROCESS_ABSENT", "got: {out:?}");
}

#[test]
fn process_present_but_port_not_listening_is_distinguished() {
    // Bind then immediately drop -- the port is very likely free again, and nothing else on this
    // box binds to an ephemeral OS-assigned port during a fast local test.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener.local_addr().expect("local_addr").port()
    };
    let out = run_probe(true, port);
    assert_eq!(out, "OBS_PORT_NOT_LISTENING", "got: {out:?}");
}

#[test]
fn process_present_and_port_listening_is_reachable() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    let out = run_probe(true, port);
    assert_eq!(out, "OBS_REACHABLE", "got: {out:?}");
    drop(listener);
}

#[test]
fn message_for_process_absent_names_the_real_cause_and_the_restart_command() {
    let msg = message_for("OBS_PROCESS_ABSENT");
    assert!(
        msg.to_lowercase().contains("not running"),
        "message must name the real cause plainly: {msg:?}"
    );
    assert!(
        msg.contains("imag-obs-start.sh"),
        "message must point at the actual restart command: {msg:?}"
    );
}

/// #1015: the LIVE 2026-08-13 finding was that imag-obs.service exists+works (installed by
/// setup-imag.sh step 21 as a systemd USER unit) but sat unsupervised because every ACTUAL
/// recovery this ticket's own preflight message drove followed its PRIMARY instruction — a
/// direct `/usr/local/bin/imag-obs-start.sh` call — which launches OBS entirely outside the
/// unit's cgroup, so `Restart=on-failure` had nothing to supervise. The supervised systemctl form
/// must be the PRIMARY instruction (named first), never a parenthetical "once supervised" aside.
#[test]
fn message_for_process_absent_leads_with_the_supervised_restart_command_1015() {
    let msg = message_for("OBS_PROCESS_ABSENT");
    let systemctl_pos = msg.find("systemctl --user start imag-obs").unwrap_or_else(|| {
        panic!(
            "message must point at the supervised systemctl restart command (issue 1015 -- a \
             direct imag-obs-start.sh recovery call bypasses Restart=on-failure supervision \
             entirely): {msg:?}"
        )
    });
    if let Some(script_pos) = msg.find("imag-obs-start.sh") {
        assert!(
            systemctl_pos < script_pos,
            "the supervised systemctl command must be the PRIMARY instruction, named BEFORE any \
             mention of the raw script -- a parenthetical \"once supervised\" aside (the old \
             wording) is exactly what led every real recovery to bypass the unit (issue 1015): {msg:?}"
        );
    }
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("never") || lower.contains("bypass"),
        "the message should explicitly warn that calling imag-obs-start.sh directly bypasses \
         supervision, not merely mention it as an interchangeable alternative: {msg:?}"
    );
}

#[test]
fn message_for_port_not_listening_names_that_distinct_cause() {
    let msg = message_for("OBS_PORT_NOT_LISTENING");
    assert!(
        msg.to_lowercase().contains("4455") && msg.to_lowercase().contains("not"),
        "message must name port 4455 not listening, distinct from process-absent: {msg:?}"
    );
}

#[test]
fn message_for_reachable_is_empty_caller_proceeds_to_the_real_attempt() {
    let msg = message_for("OBS_REACHABLE");
    assert_eq!(
        msg, "",
        "OBS_REACHABLE must yield NO fail message -- the caller proceeds to open-projectors, \
         whose own errors (handshake/auth, no matching monitor) are already accurate: got {msg:?}"
    );
}

#[test]
fn no_message_ever_hardcodes_a_wrong_connector_name() {
    for probe in [
        "OBS_PROCESS_ABSENT",
        "OBS_PORT_NOT_LISTENING",
        "OBS_REACHABLE",
    ] {
        let msg = message_for(probe);
        assert!(
            !msg.contains("DP-0") && !msg.contains("HDMI-0"),
            "no reachability message may hardcode the WRONG DP-0/HDMI-0 connector literal \
             (this box has eDP-1/HDMI-1) -- got {msg:?} for probe {probe:?}"
        );
    }
}

// ================================================================================================
// Wiring into recording-e2e.sh's [0/8] preflight: the reachability probe must run BEFORE the
// open-projectors call, and the old hardcoded DP-0/HDMI-0 fallback text must be gone.
// ================================================================================================

#[test]
fn recording_e2e_sources_the_reachability_lib() {
    let body = read(RECORDING_E2E);
    assert!(
        body.contains("lib/imag-obs-reachability.sh"),
        "recording-e2e.sh must source scripts/lib/imag-obs-reachability.sh"
    );
}

#[test]
fn recording_e2e_no_longer_hardcodes_the_wrong_dp0_hdmi0_connectors() {
    let body = read(RECORDING_E2E);
    assert!(
        !body.contains("DP-0/HDMI-0"),
        "the WRONG hardcoded 'DP-0/HDMI-0' fallback text must be gone -- this box has eDP-1/HDMI-1, \
         and the real connectors are read live, never hardcoded (#882)"
    );
}

#[test]
fn recording_e2e_reachability_probe_runs_before_open_projectors() {
    let body = read(RECORDING_E2E);
    let probe_pos = body
        .find("imag_obs_reachability_probe_cmd")
        .expect("the reachability probe call must be present in the [0/8] preflight");
    let open_pos = body
        .find("open-projectors")
        .expect("the open-projectors call must still be present");
    assert!(
        probe_pos < open_pos,
        "the reachability probe must run BEFORE open-projectors is attempted (#882) -- \
         probe_pos={probe_pos} open_pos={open_pos}"
    );
}
