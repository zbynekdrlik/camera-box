//! Behavioral guard for `scripts/dantesync-version-gate.sh` — the fleet-wide dantesync
//! VERSION-PARITY precondition gate (#862). The user's hard requirement: a fleet running mixed
//! dantesync versions (or a uniformly-STALE version) must never be discoverable only by eye or by
//! post-mortem of a failed run (that is exactly how #851's imag-nb-behind / dev1-behind drift was
//! found) — the gate must REFUSE (fail-closed) the moment ANY node's dantesync version does not
//! match the fleet's pinned expected version.
//!
//! This gate is a SEPARATE script from `version-integrity-gate.sh` (which is scoped to the
//! Windows strih/stream OBS stack only) because dantesync also runs on the Linux cam boxes AND on
//! dev1 itself (the control box that RUNS this gate) — dev1 must not be exempted just because it
//! is the harness's own host (#862 point 2: "dev1 sa nesmie vynechať").
//!
//! These tests pin the gate's own PURE functions (version extraction from `dantesync --version`
//! stdout, the per-node verdict, and the fleet-wide roll-up + table print) and its end-to-end
//! exit-code contract over fixture files (the path that needs no live rig).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/dantesync-version-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the gate (its BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout.
/// `set +e` immediately after the source neutralizes the sourced script's own leaked
/// `set -euo pipefail` (see the identical note in tests/version_integrity_gate.rs / #826) — a
/// `body` that calls a verdict function returning non-zero (a DRIFT/UNKNOWN scenario, which is
/// most of what this file asserts) must not abort the harness before it can report the result.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the gate as a subprocess WITH extra env (the fixture-injection seam); return
/// (exit_code, stdout, stderr).
fn run_gate_env(args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args).current_dir(manifest_dir());
    // #1139 hermeticity: the report-only tray + pin-lag sections do live I/O (gh release
    // download/list; ssh certutil per --win node). Seed the seams by default so the pre-existing
    // subprocess tests stay OFFLINE — a test that exercises those sections overrides them via
    // extra_env. Cover the two win node names the suite uses (strih/stream); a new win name would
    // need its own seam.
    let has = |k: &str| extra_env.iter().any(|(ek, _)| *ek == k);
    for (k, v) in [
        ("DANTESYNC_NEWEST_RELEASE", "0.0.0-hermetic"),
        ("DANTESYNC_TRAY_EXPECTED_SHA", "hermetic"),
        ("DANTESYNC_TRAY_SHA_STRIH", "hermetic"),
        ("DANTESYNC_TRAY_SHA_STREAM", "hermetic"),
    ] {
        if !has(k) {
            cmd.env(k, v);
        }
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run dantesync-version-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dantesync-version-gate-test-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// dantesync_version_from_version_output — PURE version extraction from `dantesync --version`
// stdout (#862 follow-up: the journal/service-log reader this replaced returned "" on EVERY box
// in the real fleet -- the version line it looked for is never actually logged. `dantesync
// --version` prints "dantesync X.Y.Z" and answers on every platform this gate runs against.)
// ---------------------------------------------------------------------------

#[test]
fn version_from_version_output_extracts_plain_output() {
    let out = run_sourced(
        r#"dantesync_version_from_version_output "$(printf 'dantesync 1.8.21\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "1.8.21");
}

#[test]
fn version_from_version_output_last_match_wins_amid_noise() {
    // Defensive robustness, not a real scenario: if SSH banner/MOTD noise ever precedes the real
    // line, the LAST match must win, never the first -- mirrors the "freshest wins" discipline
    // used everywhere else in this repo for a similar reason (never grade a stale/unrelated match
    // as authoritative).
    let out = run_sourced(
        r#"dantesync_version_from_version_output "$(printf 'Warning: unknown host key\ndantesync 1.8.25\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "1.8.25");
}

#[test]
fn version_from_version_output_no_match_is_empty() {
    let out = run_sourced(
        r#"dantesync_version_from_version_output "$(printf 'ssh: connect to host: Connection refused\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "");
}

#[test]
fn version_from_version_output_empty_input_is_empty() {
    let out = run_sourced(r#"dantesync_version_from_version_output """#, &[]);
    assert_eq!(out.trim(), "");
}

// ---------------------------------------------------------------------------
// dantesync_version_verdict — per-node PURE verdict + table row.
// ---------------------------------------------------------------------------

#[test]
fn verdict_ok_when_version_matches_pin() {
    let out = run_sourced(
        r#"dantesync_version_verdict cam1 1.8.21 1.8.21; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam1"), "row must name the box: {out:?}");
    assert!(
        out.contains("1.8.21"),
        "row must show the observed version: {out:?}"
    );
    assert!(out.contains("OK"), "matching pin must read OK: {out:?}");
    assert!(out.contains("RC=0"), "OK must return 0: {out:?}");
}

#[test]
fn verdict_drift_when_version_present_but_not_pin() {
    // #862 acceptance point 3: comparison is against the PIN, not just "all equal" — a present,
    // readable, but WRONG version is DRIFT even standing alone.
    let out = run_sourced(
        r#"dantesync_version_verdict imag-nb 1.8.20 1.8.21; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("imag-nb"), "row must name the box: {out:?}");
    assert!(
        out.contains("1.8.20"),
        "row must show the observed (drifted) version: {out:?}"
    );
    assert!(
        out.contains("1.8.21"),
        "row must show the expected pin: {out:?}"
    );
    assert!(
        out.contains("DRIFT"),
        "a wrong-but-readable version must be DRIFT: {out:?}"
    );
    assert!(out.contains("RC=20"), "DRIFT must return 20: {out:?}");
}

#[test]
fn verdict_unknown_when_version_unread() {
    let out = run_sourced(
        r#"dantesync_version_verdict strih "" 1.8.21; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("strih"), "row must name the box: {out:?}");
    assert!(
        out.contains("UNKNOWN"),
        "an unread version must be UNKNOWN, never a silent OK: {out:?}"
    );
    assert!(out.contains("RC=11"), "UNKNOWN must return 11: {out:?}");
}

// ---------------------------------------------------------------------------
// dantesync_fleet_report — fleet-wide roll-up, table print, CAMBOX_OFFLINE_ACK exclusion.
// ---------------------------------------------------------------------------

#[test]
fn fleet_report_all_ok_passes_and_prints_full_table() {
    let out = run_sourced(
        r#"dantesync_fleet_report 1.8.21 "cam1=1.8.21" "cam2=1.8.21" "strih=1.8.21"; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam1") && out.contains("cam2") && out.contains("strih"));
    assert!(
        out.contains("RC=0"),
        "every node on the pin must PASS: {out:?}"
    );
    assert!(
        out.to_lowercase().contains("gate pass"),
        "a clean fleet must print an explicit PASS banner: {out:?}"
    );
}

#[test]
fn fleet_report_one_drifted_node_fails_the_whole_fleet() {
    // The FAILED banner is printed to stderr (fail-loud convention every gate in this repo
    // follows) — 2>&1 on the call merges it into the captured stream without disturbing $?
    // (a bare redirect never affects the command's own exit status).
    let out = run_sourced(
        r#"dantesync_fleet_report 1.8.21 "cam1=1.8.21" "imag-nb=1.8.20" "dev1=1.8.17" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("RC=20"),
        "any DRIFT must fail the whole gate: {out:?}"
    );
    assert!(
        out.contains("imag-nb") && out.contains("1.8.20"),
        "the table must name the drifted box + its version: {out:?}"
    );
    assert!(
        out.contains("dev1") && out.contains("1.8.17"),
        "dev1 must be checked like any other node (#862 point 2): {out:?}"
    );
    assert!(
        out.contains("GATE FAILED"),
        "a drifted fleet must print an explicit FAILED banner naming the count: {out:?}"
    );
}

#[test]
fn fleet_report_uniformly_stale_fleet_still_fails_against_the_pin() {
    // #862 acceptance point 3, at fleet scale: EVERY node agreeing with each other on an OLD
    // version must still fail — this is not a "boxes disagree" check, it is a PIN check.
    let out = run_sourced(
        r#"dantesync_fleet_report 1.8.21 "cam1=1.8.19" "cam2=1.8.19" "cam3=1.8.19" "cam4=1.8.19"; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("RC=20"),
        "a fleet uniformly on a stale version must still FAIL against the pin, not pass because they agree: {out:?}"
    );
}

#[test]
fn fleet_report_unread_node_is_unknown_not_a_silent_pass() {
    let out = run_sourced(
        r#"dantesync_fleet_report 1.8.21 "cam1=1.8.21" "cam3="; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("RC=11"),
        "an unread node with no drift present must be UNKNOWN (11), never OK: {out:?}"
    );
    assert!(out.contains("UNKNOWN"));
}

#[test]
fn fleet_report_acked_offline_node_is_excluded_not_judged() {
    // #862 point 4: an unavailable node must not silently pass, but a KNOWINGLY offline one
    // (the SAME CAMBOX_OFFLINE_ACK mechanism recording-e2e.sh already uses, #758/#827) must be
    // reported EXCLUDED — never counted as UNKNOWN/DRIFT, and never a reason to fail the gate.
    let out = run_sourced(
        r#"dantesync_fleet_report 1.8.21 "cam1=1.8.21" "cam5="; echo "RC=$?""#,
        &[("CAMBOX_OFFLINE_ACK", "cam5:powered-off-2026-07-27")],
    );
    assert!(
        out.contains("RC=0"),
        "an acked-offline node must not fail the gate: {out:?}"
    );
    assert!(
        out.contains("cam5") && out.to_uppercase().contains("EXCLUDED"),
        "the acked node must be visibly EXCLUDED in the table: {out:?}"
    );
    assert!(
        out.contains("powered-off-2026-07-27"),
        "the exclusion row must carry the ack REASON, not just the fact of exclusion: {out:?}"
    );
}

#[test]
fn fleet_report_prints_a_box_to_version_table_on_failure() {
    // #862 point 5: the rejection message must print a box->version table, never just "version
    // mismatch".
    let out = run_sourced(
        r#"dantesync_fleet_report 1.8.21 "cam1=1.8.21" "cam2=1.8.21" "cam3=1.8.21" "cam4=1.8.21" "imag-nb=1.8.20" "dev1=1.8.17" "strih=1.8.21" "stream=1.8.21"; echo "RC=$?""#,
        &[],
    );
    for name in [
        "cam1", "cam2", "cam3", "cam4", "imag-nb", "dev1", "strih", "stream",
    ] {
        assert!(
            out.contains(name),
            "table must list every node, missing {name}: {out:?}"
        );
    }
    assert!(out.contains("RC=20"));
}

// ---------------------------------------------------------------------------
// End-to-end CLI: --win, --local, and --linux, all reading `dantesync --version` output via
// fixture injection (no live rig, no bundle-state coupling -- #862 follow-up dropped --win-state
// entirely: strih/stream are read the SAME way as every other node now, over SSH).
// ---------------------------------------------------------------------------

#[test]
fn cli_win_node_reads_dantesync_version_via_fixture_override() {
    let dir = tmp_dir("win");
    let path = dir.join("strih.out");
    std::fs::write(&path, "dantesync 1.8.21\n").unwrap();
    let (code, out, err) = run_gate_env(
        &["--pin", "1.8.21", "--win", "strih=newlevel@10.77.9.202"],
        &[(
            "DANTESYNC_VERSION_GATE_VERSION_STRIH",
            path.to_str().unwrap(),
        )],
    );
    assert_eq!(code, 0, "stdout={out}\nstderr={err}");
    assert!(out.contains("strih") && out.contains("1.8.21"));
}

#[test]
fn cli_win_node_unreachable_is_unknown_never_a_silent_pass() {
    // No fixture override at all -- the real ssh call will fail against an unroutable/loopback
    // address in the test sandbox (no live rig here), which must read as UNKNOWN, never a pass.
    let (code, out, _err) = run_gate_env(
        &["--pin", "1.8.21", "--win", "strih=nobody@127.0.0.1"],
        &[(
            "DANTESYNC_VERSION_GATE_SSH_TIMEOUT",
            "1", // keep the test fast -- fail closed quickly rather than waiting out a real timeout
        )],
    );
    assert_eq!(
        code, 11,
        "an unreachable --win node must be UNKNOWN, never a silent pass: {out}"
    );
}

#[test]
fn cli_local_node_reads_dantesync_version_via_fixture_override() {
    let dir = tmp_dir("local");
    let path = dir.join("dev1.out");
    std::fs::write(&path, "dantesync 1.8.17\n").unwrap();
    let (code, out, err) = run_gate_env(
        &["--pin", "1.8.21", "--local", "dev1"],
        &[(
            "DANTESYNC_VERSION_GATE_VERSION_DEV1",
            path.to_str().unwrap(),
        )],
    );
    assert_eq!(code, 20, "stdout={out}\nstderr={err}");
    assert!(out.contains("dev1") && out.contains("1.8.17"));
}

#[test]
fn cli_linux_node_reads_dantesync_version_via_fixture_override() {
    let dir = tmp_dir("linux");
    let path = dir.join("cam1.out");
    std::fs::write(&path, "dantesync 1.8.21\n").unwrap();
    let (code, out, err) = run_gate_env(
        &["--pin", "1.8.21", "--linux", "cam1=root@10.77.9.61"],
        &[(
            "DANTESYNC_VERSION_GATE_VERSION_CAM1",
            path.to_str().unwrap(),
        )],
    );
    assert_eq!(code, 0, "stdout={out}\nstderr={err}");
    assert!(out.contains("cam1") && out.contains("1.8.21"));
}

#[test]
fn cli_zero_nodes_is_a_usage_error() {
    let (code, _out, err) = run_gate_env(&["--pin", "1.8.21"], &[]);
    assert_eq!(
        code, 1,
        "no nodes at all must be refused as a usage error, never a silent pass"
    );
    assert!(err.to_lowercase().contains("no node"));
}

// ---------------------------------------------------------------------------
// #1139 — REPORT-ONLY orphan alarms: dantesync-tray.exe sha-pin + pin-vs-newest-release lag.
// The tray is in NO gate (a daemon roll can leave the tray behind — live 2026-08-20 the deployed
// tray sha != the pinned v1.8.48 release asset), and the fixed DANTESYNC_VERSION_PIN can lag the
// newest published release. Both SCREAM (report-only) but never flip the gate exit — the tray is a
// cosmetic status GUI with no clock role, the pin lag reflects a deliberate canary rollout. The
// tray is pinned by sha256 against the release asset (#1118 pattern) because it is a GUI-subsystem
// app with no console --version. Pure verdicts unit-tested by sourcing; report-only wiring by CLI.
// ---------------------------------------------------------------------------

#[test]
fn tray_verdict_ok_when_deployed_matches_pinned_release() {
    let out = run_sourced(
        r#"o="$(dantesync_tray_verdict strih "35d9e631d25f" "35d9e631d25f")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(out.contains("RC=0"), "matching tray must be OK: {out}");
    assert!(out.contains("OK"), "{out}");
}

#[test]
fn tray_verdict_alarms_when_deployed_lags_pinned_release() {
    let out = run_sourced(
        r#"o="$(dantesync_tray_verdict strih "35d9e631d25f" "bd45be05c18c")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=30"),
        "a lagging tray must return the ALARM code 30: {out}"
    );
    assert!(out.contains("ALARM"), "must SCREAM ALARM: {out}");
    assert!(
        out.contains("bd45be05c18c"),
        "must name the expected pinned sha: {out}"
    );
}

#[test]
fn tray_verdict_unknown_fail_closed_when_deployed_or_expected_unread() {
    let a = run_sourced(
        r#"o="$(dantesync_tray_verdict strih "" "bd45be05")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        a.contains("RC=31") && a.contains("UNKNOWN"),
        "unread deployed tray -> UNKNOWN(31): {a}"
    );
    let b = run_sourced(
        r#"o="$(dantesync_tray_verdict strih "35d9e631" "")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        b.contains("RC=31") && b.contains("UNKNOWN"),
        "unresolved expected sha -> UNKNOWN(31): {b}"
    );
}

#[test]
fn pin_lag_verdict_ok_when_pin_is_newest() {
    let out = run_sourced(
        r#"o="$(dantesync_pin_lag_verdict "1.8.48" "1.8.48")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=0") && out.contains("OK"),
        "pin==newest -> OK: {out}"
    );
}

#[test]
fn pin_lag_verdict_alarms_when_pin_behind_newest() {
    let out = run_sourced(
        r#"o="$(dantesync_pin_lag_verdict "1.8.48" "1.8.49")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=32"),
        "a lagging pin must return the LAG code 32: {out}"
    );
    assert!(out.contains("LAG"), "must SCREAM LAG: {out}");
    assert!(
        out.contains("1.8.49"),
        "must name the newest published release: {out}"
    );
}

#[test]
fn pin_lag_verdict_unknown_fail_closed_when_newest_unresolved() {
    let out = run_sourced(
        r#"o="$(dantesync_pin_lag_verdict "1.8.48" "")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        out.contains("RC=33") && out.contains("UNKNOWN"),
        "unresolved newest -> UNKNOWN(33): {out}"
    );
}

#[test]
fn tray_alarm_is_report_only_does_not_block_an_otherwise_clean_gate() {
    // A clean daemon (matches pin) + a LAGGING tray must still PASS (exit 0): the tray ALARM is
    // report-only. NEWEST==pin isolates the tray alarm from the pin-lag alarm.
    let dir = tmp_dir("tray_report_only");
    let path = dir.join("strih.out");
    std::fs::write(&path, "dantesync 1.8.48\n").unwrap();
    let (code, out, err) = run_gate_env(
        &["--pin", "1.8.48", "--win", "strih=newlevel@10.77.9.202"],
        &[
            (
                "DANTESYNC_VERSION_GATE_VERSION_STRIH",
                path.to_str().unwrap(),
            ),
            ("DANTESYNC_TRAY_SHA_STRIH", "35d9e631d25fdeadbeef"),
            ("DANTESYNC_TRAY_EXPECTED_SHA", "bd45be05c18cdeadbeef"),
            ("DANTESYNC_NEWEST_RELEASE", "1.8.48"),
        ],
    );
    assert_eq!(
        code, 0,
        "a lagging TRAY must be report-only (gate still passes). out={out} err={err}"
    );
    assert!(
        out.contains("GATE PASS"),
        "daemon-clean gate must pass: {out}"
    );
    assert!(
        out.contains("dantesync-tray.exe sha-pin") && out.contains("ALARM"),
        "the tray section must SCREAM the drift: {out}"
    );
    assert!(
        err.contains("DANTESYNC-TRAY ALARM"),
        "a loud stderr banner must fire: {err}"
    );
}

#[test]
fn pin_lag_alarm_is_report_only_does_not_block_an_otherwise_clean_gate() {
    // A clean daemon + a clean tray + a pin BEHIND the newest release must still PASS (exit 0):
    // the lag alarm is report-only.
    let dir = tmp_dir("lag_report_only");
    let path = dir.join("strih.out");
    std::fs::write(&path, "dantesync 1.8.48\n").unwrap();
    let (code, out, err) = run_gate_env(
        &["--pin", "1.8.48", "--win", "strih=newlevel@10.77.9.202"],
        &[
            (
                "DANTESYNC_VERSION_GATE_VERSION_STRIH",
                path.to_str().unwrap(),
            ),
            ("DANTESYNC_TRAY_SHA_STRIH", "cafe"),
            ("DANTESYNC_TRAY_EXPECTED_SHA", "cafe"),
            ("DANTESYNC_NEWEST_RELEASE", "1.8.49"),
        ],
    );
    assert_eq!(
        code, 0,
        "a lagging PIN must be report-only (gate still passes). out={out} err={err}"
    );
    assert!(out.contains("GATE PASS"), "{out}");
    assert!(
        out.contains("pin vs newest") && out.contains("LAG") && out.contains("1.8.49"),
        "the pin-lag section must SCREAM the lag: {out}"
    );
    assert!(
        err.contains("DANTESYNC-PIN-LAG ALARM"),
        "a loud stderr banner must fire: {err}"
    );
}
