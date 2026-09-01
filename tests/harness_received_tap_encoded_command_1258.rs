//! #1258 — the `[4c/8]` frozen-camera gate's `received=` tap read strih's OBS log via
//! `mv_reverify_probe_raw`, which invoked PowerShell over ssh with the NAIVE triple-quoted
//! `-Command "gc (gci ... | sort ... | select ...)..."` string. Win32-OpenSSH's default cmd.exe
//! shell MANGLES that (the bash -> ssh -> cmd.exe -> powershell three-layer quoting hazard
//! win-ssh-exec.sh documents): the unescaped `|` pipes leak to cmd.exe, so the read returned
//! non-tail noise and EVERY source read `received=none` on EVERY attempt of EVERY run since #1233
//! (run 33513175938 + the 4 prior "green" runs — all 4/4 INCONCLUSIVE) — the frozen-camera abort
//! gate silently never bit; only the downstream QR sweep protected the run.
//!
//! The fix invokes PowerShell via `-EncodedCommand` (base64 UTF-16LE), the cmd.exe-proof mechanism
//! win-ssh-exec.sh's `win_ssh_run` already uses (the base64 blob is pure ASCII with no shell-special
//! chars). All Tier-0 (no rig, no real ssh): a fake `sshpass` on PATH echoes its argv, so the
//! function's stdout IS the ssh/powershell invocation the real read would build; the test asserts it
//! is an `-EncodedCommand` invocation whose payload decodes back to the intended
//! `gc (gci ...).FullName -Tail N` command (pipes carried intact), NOT the naive `-Command "gc ..."`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    manifest_dir().join("scripts/lib/mv-reverify-escalate.sh")
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Run `mv_reverify_probe_raw` under a restricted PATH with a fake `sshpass` that echoes its full
/// argv, so the returned stdout IS the ssh/powershell invocation shape the real read builds. The
/// `#833`/painter-up fake-binary-on-PATH pattern (harness_mv_reverify_escalate_1093.rs).
fn probe_raw_invocation() -> String {
    let dir = std::env::temp_dir().join(format!(
        "received_tap_1258_{}_{}",
        std::process::id(),
        nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let sshpass = dir.join("sshpass");
    fs::write(&sshpass, "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\"\n").unwrap();
    let mut perms = fs::metadata(&sshpass).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&sshpass, perms).unwrap();

    let script = format!(
        "set -uo pipefail\n. \"{}\" 2>/dev/null\nmv_reverify_probe_raw 10.0.0.1 'NDI cam1'",
        lib_path().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .expect("run bash");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn received_tap_uses_encoded_command_not_naive_quoting_1258() {
    let inv = probe_raw_invocation();
    assert!(
        inv.contains("-EncodedCommand "),
        "#1258: the received= tap must invoke PowerShell via -EncodedCommand (cmd.exe-proof), not \
         the naive -Command \"...\" string Win32-OpenSSH's cmd.exe mangles. Got: {inv}"
    );
    assert!(
        !inv.contains("-Command \"gc"),
        "#1258: the naive `-Command \"gc (gci ... | sort ... | select ...)\"` form leaks its `|` \
         pipes to cmd.exe -> every source reads `received=none` (the 4/4 INCONCLUSIVE bug). Got: {inv}"
    );
}

#[test]
fn received_tap_encoded_payload_decodes_to_the_obs_log_tail_command_1258() {
    let inv = probe_raw_invocation();
    let b64 = inv
        .split("-EncodedCommand ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("");
    assert!(
        !b64.is_empty(),
        "#1258: no -EncodedCommand payload in the invocation: {inv}"
    );
    // base64 alphabet is [A-Za-z0-9+/=] only (no shell-special chars) -> safe to single-quote.
    let decoded = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s' '{b64}' | base64 -d | iconv -f UTF-16LE -t UTF-8"
        ))
        .output()
        .expect("decode");
    let ps = String::from_utf8_lossy(&decoded.stdout).to_string();
    assert!(
        ps.contains("gci $env:APPDATA\\obs-studio\\logs\\*.txt")
            && ps.contains("| sort LastWriteTime")
            && ps.contains("| select -last 1")
            && ps.contains(".FullName -Tail"),
        "#1258: the -EncodedCommand payload must decode to the OBS-log tail read command with its \
         pipes carried intact through base64. Got: {ps}"
    );
}
