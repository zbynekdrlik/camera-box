//! #1259 — fleet-wide migration of the OTHER naive `powershell -NoProfile -Command "…| sort …|
//! select …"` OBS-log reads over ssh to `-EncodedCommand`, the #1258 follow-up. Win32-OpenSSH's
//! default cmd.exe shell on strih/stream MANGLES the naive triple-quoted form (the bash -> ssh ->
//! cmd.exe -> powershell three-layer quoting hazard win-ssh-exec.sh documents): the unescaped `|`
//! pipes leak to cmd.exe -> the read returns non-tail noise -> a blind/empty result. #1258 fixed
//! ONLY `mv_reverify_probe_raw`; this proves the 6 SHELL sites now emit `-EncodedCommand`:
//!   * the shared helper `scripts/lib/ps-encoded.sh::ps_encoded_command` (base64 UTF-16LE, round-trip);
//!   * `scripts/asio-starve-alert-watchdog.sh` `fetch_box_log`;
//!   * `scripts/frozen-input-alert-watchdog.sh` `probe_received`;
//!   * `scripts/ndi-halving-watchdog.sh` `fetch_box_log`;
//!   * `scripts/cadence-alert-watchdog.sh` `fetch_box_log`;
//!   * `scripts/mv-fps-alert-watchdog.sh` `probe_mv_log` (win branch);
//!   * `scripts/lib/mv-fps-preflight.sh` `mv_fps_preflight_read_cmd` (win branch, LIVE `[4d1/8]`).
//!
//! (The 2 Python sites in scripts/rig-health-audit.py are covered by
//! tests/python/test_rig_health_audit_encoded_1259.py — Python cannot source a bash lib.)
//!
//! All Tier-0 (no rig, no real ssh): a fake `sshpass` on PATH appends its argv to a log file (the
//! #833/#1258 fake-binary-on-PATH pattern), so the log IS the ssh/powershell invocation the real
//! read builds. The tests assert the invocation carries `-NoProfile -NonInteractive -EncodedCommand`
//! (never the naive `-Command "gc`/`-Command "$f`) and that the base64 payload decodes back to the
//! intended PowerShell with its pipes carried intact.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ps_enc_1259_{}_{}_{}",
        tag,
        std::process::id(),
        nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// Source `script_rel` and run `call`; a fake `sshpass` on PATH appends its argv (one element per
/// line) to a log file and prints nothing to stdout — so the returned string is every ssh/powershell
/// invocation the call built (the last argv element of each ssh is the whole `powershell …` command).
fn ssh_invocation(script_rel: &str, call: &str) -> String {
    let dir = tmp_dir("ssh");
    let sshpass = dir.join("sshpass");
    write_exec(
        &sshpass,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >> \"$SSHPASS_ARGV_LOG\"\n",
    );
    let log = dir.join("argv.log");
    fs::write(&log, "").unwrap();
    let script = manifest_dir().join(script_rel);
    let bash = format!("set -uo pipefail\n. \"{}\"\n{}\n", script.display(), call);
    // Force the REAL ssh branch: clear any *_PROBE_CMD stub override that might leak from the CI env
    // (else the watchdog takes the probe branch and the test fails with a confusing "no
    // -EncodedCommand payload" — fail-loud, never a silent pass; #1259 review).
    let _ = Command::new("bash")
        .arg("-c")
        .arg(&bash)
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .env("SSHPASS_ARGV_LOG", &log)
        .env_remove("ASIO_STARVE_PROBE_CMD")
        .env_remove("FROZEN_INPUT_PROBE_CMD")
        .env_remove("NDI_HALVING_PROBE_CMD")
        .env_remove("CADENCE_PROBE_CMD")
        .env_remove("MV_FPS_PROBE_CMD")
        .output()
        .expect("run bash");
    let logged = fs::read_to_string(&log).unwrap_or_default();
    let _ = fs::remove_dir_all(&dir);
    logged
}

/// Source `script_rel` and run `call`, returning its stdout (for a pure command-BUILDER that prints
/// the remote command string itself, e.g. mv_fps_preflight_read_cmd).
fn stdout_of(script_rel: &str, call: &str) -> String {
    let script = manifest_dir().join(script_rel);
    let bash = format!("set -uo pipefail\n. \"{}\"\n{}\n", script.display(), call);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&bash)
        .output()
        .expect("run bash");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The single `powershell … -EncodedCommand <b64>` line's base64 payload, decoded back to PS text.
fn decode_encoded_payload(invocation: &str) -> String {
    let b64 = invocation
        .split("-EncodedCommand ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("");
    assert!(
        !b64.is_empty(),
        "#1259: no -EncodedCommand payload in the invocation: {invocation}"
    );
    // base64 alphabet is [A-Za-z0-9+/=] only (no shell-special chars) -> safe to single-quote.
    let decoded = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s' '{b64}' | base64 -d | iconv -f UTF-16LE -t UTF-8"
        ))
        .output()
        .expect("decode");
    String::from_utf8_lossy(&decoded.stdout).to_string()
}

fn assert_encoded_not_naive(inv: &str, ctx: &str) {
    assert!(
        inv.contains("-NoProfile -NonInteractive -EncodedCommand "),
        "#1259: {ctx} must invoke PowerShell -NoProfile -NonInteractive -EncodedCommand (cmd.exe-proof), \
         not the naive -Command \"...\" Win32-OpenSSH's cmd.exe mangles. Got: {inv}"
    );
    assert!(
        !inv.contains("-Command \"gc") && !inv.contains("-Command \"$f") && !inv.contains("-Command \"\\$f"),
        "#1259: {ctx} must not use the naive `-Command \"gc/$f …| sort …| select …\"` form (its `|` \
         pipes leak to cmd.exe -> blind read). Got: {inv}"
    );
}

// The four structurally-identical simple watchdog reads share one payload:
//   gc (gci $env:APPDATA\obs-studio\logs\*.txt | sort LastWriteTime | select -last 1).FullName -Tail N
fn assert_simple_tail_payload(ps: &str, ctx: &str) {
    assert!(
        ps.contains("gc (gci $env:APPDATA\\obs-studio\\logs\\*.txt")
            && ps.contains("| sort LastWriteTime")
            && ps.contains("| select -last 1")
            && ps.contains(").FullName -Tail"),
        "#1259: {ctx} payload must decode to the newest-OBS-log tail read with its pipes intact. Got: {ps}"
    );
}

#[test]
fn ps_encoded_command_round_trips_arbitrary_powershell_1259() {
    let sample =
        "gc (gci $env:APPDATA\\x | sort LastWriteTime | select -last 1).FullName -Tail 400";
    let inv = stdout_of(
        "scripts/lib/ps-encoded.sh",
        &format!("ps_encoded_command '{sample}'"),
    );
    assert!(
        !inv.is_empty(),
        "#1259: ps_encoded_command must print a base64 blob (the shared helper is missing/empty)"
    );
    let decoded = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s' '{inv}' | base64 -d | iconv -f UTF-16LE -t UTF-8"
        ))
        .output()
        .expect("decode");
    let back = String::from_utf8_lossy(&decoded.stdout).to_string();
    assert_eq!(
        back, sample,
        "#1259: ps_encoded_command must base64-UTF16LE-encode its input so it round-trips exactly \
         (pipes/$/backslashes carried through)."
    );
}

#[test]
fn ps_clamp_numeric_guards_the_tail_count_1259() {
    // A bare non-negative integer passes through; empty / non-digit / negative / decimal -> default.
    // This is the guard the 5 watchdogs apply to $OBS_LOG_TAIL before it enters the encoded payload.
    let cases = [
        ("800", "1200", "800"),
        ("", "1200", "1200"),
        ("800; Remove-Item C:\\evil", "1200", "1200"),
        ("-5", "1200", "1200"),
        ("12.5", "1200", "1200"),
    ];
    for (val, def, want) in cases {
        let got = stdout_of(
            "scripts/lib/ps-encoded.sh",
            &format!("ps_clamp_numeric '{val}' {def}"),
        );
        assert_eq!(
            got, want,
            "ps_clamp_numeric '{val}' {def} must clamp to {want}"
        );
    }
}

#[test]
fn asio_starve_fetch_box_log_uses_encoded_command_1259() {
    let inv = ssh_invocation(
        "scripts/asio-starve-alert-watchdog.sh",
        "fetch_box_log 10.0.0.1",
    );
    assert_encoded_not_naive(&inv, "asio-starve fetch_box_log");
    assert_simple_tail_payload(&decode_encoded_payload(&inv), "asio-starve fetch_box_log");
}

#[test]
fn frozen_input_probe_received_uses_encoded_command_1259() {
    let inv = ssh_invocation(
        "scripts/frozen-input-alert-watchdog.sh",
        "probe_received 10.0.0.1 'NDI cam1'",
    );
    assert_encoded_not_naive(&inv, "frozen-input probe_received");
    assert_simple_tail_payload(&decode_encoded_payload(&inv), "frozen-input probe_received");
}

#[test]
fn ndi_halving_fetch_box_log_uses_encoded_command_1259() {
    let inv = ssh_invocation("scripts/ndi-halving-watchdog.sh", "fetch_box_log 10.0.0.1");
    assert_encoded_not_naive(&inv, "ndi-halving fetch_box_log");
    assert_simple_tail_payload(&decode_encoded_payload(&inv), "ndi-halving fetch_box_log");
}

#[test]
fn cadence_fetch_box_log_uses_encoded_command_1259() {
    let inv = ssh_invocation(
        "scripts/cadence-alert-watchdog.sh",
        "fetch_box_log 10.0.0.1",
    );
    assert_encoded_not_naive(&inv, "cadence fetch_box_log");
    assert_simple_tail_payload(&decode_encoded_payload(&inv), "cadence fetch_box_log");
}

#[test]
fn mv_fps_alert_probe_mv_log_win_uses_encoded_command_1259() {
    let inv = ssh_invocation(
        "scripts/mv-fps-alert-watchdog.sh",
        "probe_mv_log 10.0.0.1 win",
    );
    assert_encoded_not_naive(&inv, "mv-fps-alert probe_mv_log(win)");
    let ps = decode_encoded_payload(&inv);
    assert!(
        ps.contains("$f=(gci $env:APPDATA\\obs-studio\\logs\\*.txt")
            && ps.contains("| sort LastWriteTime")
            && ps.contains("| select -last 1")
            && ps.contains("if($f)")
            && ps.contains("MVFPS_LOGID:")
            && ps.contains("gc $f.FullName -Tail"),
        "#1259: mv-fps-alert probe_mv_log(win) payload must decode to the MVFPS_LOGID + tail read. Got: {ps}"
    );
}

#[test]
fn mv_fps_preflight_read_cmd_win_uses_encoded_command_1259() {
    // The win branch of the pure builder prints the whole remote command string.
    let inv = stdout_of(
        "scripts/lib/mv-fps-preflight.sh",
        "mv_fps_preflight_read_cmd win 800",
    );
    assert_encoded_not_naive(&inv, "mv-fps-preflight read_cmd(win)");
    let ps = decode_encoded_payload(&inv);
    assert!(
        ps.contains("$f=(gci $env:APPDATA\\obs-studio\\logs\\*.txt")
            && ps.contains("| sort LastWriteTime")
            && ps.contains("| select -last 1")
            && ps.contains("if($f)")
            && ps.contains("gc $f.FullName -Tail 800"),
        "#1259: mv-fps-preflight read_cmd(win 800) payload must decode to the tail read (-Tail 800). Got: {ps}"
    );
}

#[test]
fn mv_fps_preflight_read_cmd_linux_is_unchanged_bash_1259() {
    // The linux branch must stay a plain bash tail (never encoded) — only win crosses cmd.exe.
    let inv = stdout_of(
        "scripts/lib/mv-fps-preflight.sh",
        "mv_fps_preflight_read_cmd linux 800",
    );
    assert!(
        inv.contains("tail -n 800") && !inv.contains("-EncodedCommand"),
        "#1259: the linux read_cmd branch must remain a plain bash tail (no cmd.exe boundary). Got: {inv}"
    );
}
