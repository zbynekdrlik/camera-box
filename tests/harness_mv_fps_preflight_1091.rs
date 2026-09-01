//! issue 1091 (issue 771 point 3) — the E2E-preflight MV-fps floor read.
//!
//! Root cause (issue 771 / issue 1083): vendored libobs `render_display()` emits
//! `multiview-audit: monitor=N divisor=D rendered_fps=X target=Z floor=F …` ~every 5 s per
//! throttleable Multiview projector; `src/mv_audit.rs` parses+gates it, the `mv-fps-gate` bin is the
//! decision engine, and issue 1083 shipped the LIVE always-on dev1 watchdog over it. But the E2E gate
//! never read the audit line — a box whose Multiview render already collapsed (imag monitor-3 ~12fps
//! for 5 min, strih 4K MV 9–11fps under contention) still ran, wasting a ~40-min recording. This
//! wires the SYNCHRONOUS gate-time consumer: `scripts/lib/mv-fps-preflight.sh` reads each OBS box's
//! newest log, runs `$PROBE_BIN_DIR/mv-fps-gate` over the latest samples, and fails loud on a
//! CONFIRMED below-floor collapse — while NEVER false-aborting a CI gate on a transient / unreadable
//! box (the user's hardest constraint).
//!
//! Same convention as `tests/mv_fps_alert_watchdog_1083.rs`: source the REAL lib (source-only, no
//! side effects) and drive the pure functions + the assert with a FAKE probe (`MV_FPS_PREFLIGHT_
//! PROBE_CMD`) + a FAKE gate binary — no ssh, no Rust binary, no rig. The lib runs under the caller's
//! `set -euo pipefail` in every harness, so a non-zero gate exit that tripped `-e` would fail the
//! test. RED before the lib / wiring exist; GREEN after.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &str) -> String {
    let path = manifest_dir().join(p);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn lib_path() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/mv-fps-preflight.sh");
    assert!(s.exists(), "{} not found (issue 1091)", s.display());
    s
}

/// Source the REAL lib UNDER `set -euo pipefail` (the caller's environment) and run `body`.
/// Returns (exit, stdout, stderr).
fn run_lib(body: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .current_dir(manifest_dir());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lib_stdout(body: &str) -> String {
    let (rc, out, err) = run_lib(body, &[]);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out
}

// -------------------------------------------------------------------------------------------
// lib shape + the pure remote-read builders
// -------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "mv_fps_preflight_read_cmd",
        "mv_fps_preflight_probe",
        "mv_fps_preflight_assert",
    ] {
        let out = lib_stdout(&format!("type {f} >/dev/null 2>&1 && echo OK"));
        assert_eq!(out.trim(), "OK", "{f} is not defined by the preflight lib");
    }
}

#[test]
fn read_cmd_linux_tails_the_newest_obs_log() {
    let out = lib_stdout("mv_fps_preflight_read_cmd linux 500");
    assert!(
        out.contains(".config/obs-studio/logs/*.txt"),
        "linux read must target the OBS log dir: {out}"
    );
    assert!(
        out.contains("ls -t"),
        "linux read must pick the NEWEST log: {out}"
    );
    assert!(
        out.contains("tail -n 500"),
        "linux read must honour the passed tail count: {out}"
    );
}

#[test]
fn read_cmd_win_is_a_single_non_nested_powershell() {
    // #1259: the win read now goes over cmd.exe-proof -EncodedCommand (base64 UTF-16LE), NEVER the
    // naive -Command "…| sort …" that Win32-OpenSSH's default cmd.exe shell mangles (the issue-1258
    // root cause — the `|` pipes leak to cmd.exe -> a blind read). The APPDATA/obs-studio path + the
    // -Tail count now live INSIDE the base64 payload, not the literal command line, so decode it and
    // assert they survive intact.
    let out = lib_stdout("mv_fps_preflight_read_cmd win 500");
    assert!(
        out.contains("powershell -NoProfile -NonInteractive -EncodedCommand "),
        "win read must be a -EncodedCommand powershell (cmd.exe-proof), not naive -Command: {out}"
    );
    assert!(
        !out.contains("-Command \""),
        "win read must NOT use the naive -Command \"…\" form (its pipes leak to cmd.exe): {out}"
    );
    // win-ssh-vs-mcp / rig-state-inspection: ONE flat, non-nested powershell (a nested powershell
    // over ssh fails SILENTLY on these boxes).
    assert_eq!(
        out.matches("powershell").count(),
        1,
        "win read must be a SINGLE, non-nested powershell: {out}"
    );
    let b64 = out
        .split("-EncodedCommand ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("");
    assert!(!b64.is_empty(), "no -EncodedCommand payload: {out}");
    let decoded = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s' '{b64}' | base64 -d | iconv -f UTF-16LE -t UTF-8"
        ))
        .output()
        .expect("decode");
    let ps = String::from_utf8_lossy(&decoded.stdout).to_string();
    assert!(
        ps.to_uppercase().contains("APPDATA")
            && ps.contains("obs-studio\\logs")
            && ps.contains("-Tail 500"),
        "the decoded -EncodedCommand payload must target %APPDATA%\\obs-studio\\logs with -Tail 500: {ps}"
    );
}

#[test]
fn read_cmd_unknown_os_fails() {
    // An unknown os must return non-zero so the caller treats the box as unreadable (UNKNOWN),
    // never silently emitting an empty command that would read as "no audit line".
    let (rc, out, err) = run_lib("mv_fps_preflight_read_cmd macos 100 || echo REJECTED", &[]);
    assert_eq!(rc, 0, "harness itself must run: {err}");
    assert!(
        out.contains("REJECTED"),
        "unknown os must fail (non-zero): {out}"
    );
}

// -------------------------------------------------------------------------------------------
// wiring — recording-e2e.sh sources the lib + calls the assert with the PROBE_BIN_DIR gate,
// positioned AFTER PROBE_BIN_DIR is resolved and BEFORE the [4d/8] render-budget gate.
// -------------------------------------------------------------------------------------------
#[test]
fn recording_e2e_sh_wires_the_preflight_before_the_render_budget_gate() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("lib/mv-fps-preflight.sh"),
        "recording-e2e.sh must source scripts/lib/mv-fps-preflight.sh (issue 1091)"
    );
    let call = s
        .find("mv_fps_preflight_assert")
        .expect("recording-e2e.sh must call mv_fps_preflight_assert (issue 1091)");
    let win = &s[call..(call + 600).min(s.len())];
    assert!(
        win.contains("PROBE_BIN_DIR/mv-fps-gate"),
        "the preflight must consume the gate binary via $PROBE_BIN_DIR/mv-fps-gate (issue 1091). Got:\n{win}"
    );
    assert!(
        win.contains("strih") && win.contains("imag"),
        "the preflight must read BOTH strih + imag (issue 1091). Got:\n{win}"
    );
    let probe_dir = s
        .find("PROBE_BIN_DIR=")
        .expect("recording-e2e.sh must set PROBE_BIN_DIR");
    let rb_banner = s
        .find("[4d/8] #405")
        .expect("recording-e2e.sh must still have the [4d/8] render-budget banner");
    assert!(
        probe_dir < call,
        "the MV-fps preflight must be wired AFTER PROBE_BIN_DIR is resolved (it consumes it)"
    );
    assert!(
        call < rb_banner,
        "the MV-fps preflight must run BEFORE the [4d/8] render-budget gate (a collapsed MV must \
         fail-fast, not waste the render-budget window)"
    );
}

// -------------------------------------------------------------------------------------------
// assert behaviour — fake probe + fake gate, run UNDER `set -euo pipefail`, reprobe sleep 0
// -------------------------------------------------------------------------------------------

/// A fake probe that ignores its <ip> <os> args and prints $FAKE_PROBE_OUT.
fn write_fake_probe(dir: &Path) -> PathBuf {
    let p = dir.join("fake-probe.sh");
    std::fs::write(
        &p,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"${FAKE_PROBE_OUT:-}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

/// A fake gate: consumes stdin, then picks the Nth exit code from the space-separated
/// $FAKE_GATE_EXITS (clamped to the last) using a per-test $FAKE_GATE_COUNTER file — so ONE binary
/// can drive "always 1" (sustained), "1 0" (transient recovery), "0" (pass), "2" (unclassifiable).
/// Emits a `FAIL …` line on exit 1 (what the real gate prints), so the assert's detail extraction is
/// exercised too.
fn write_fake_gate(dir: &Path) -> PathBuf {
    let p = dir.join("fake-gate.sh");
    std::fs::write(
        &p,
        r#"#!/usr/bin/env bash
cat >/dev/null 2>&1
cf="${FAKE_GATE_COUNTER:?}"
n=0; [ -f "$cf" ] && n="$(cat "$cf" 2>/dev/null || echo 0)"
echo $((n + 1)) > "$cf"
read -r -a arr <<< "${FAKE_GATE_EXITS:-0}"
idx="$n"; [ "$idx" -ge "${#arr[@]}" ] && idx=$(( ${#arr[@]} - 1 ))
ec="${arr[$idx]}"
[ "$ec" = "1" ] && echo "FAIL monitor=1 divisor=1 rendered_fps=9.0 < floor=28.0 (target 30, 3840x2160)"
exit "$ec"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

const AUDIT_LINE: &str = "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=28.0 cx=3840 cy=2160";

/// Run `mv_fps_preflight_assert` for ONE box under `set -euo pipefail` with a fake probe + fake gate.
/// `probe_out` is what the fake probe prints; `gate_exits` is the space-separated exit sequence.
/// Returns (exit, stdout, stderr). A trailing `echo PROCEEDED` proves the assert RETURNED (did not
/// `exit 1`).
fn run_assert(probe_out: &str, gate_exits: &str) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let probe = write_fake_probe(dir.path());
    let gate = write_fake_gate(dir.path());
    let counter = dir.path().join("gate.counter");
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\nmv_fps_preflight_assert \"$GATE\" \"strih|10.0.0.1|win|u|p\"\necho PROCEEDED")
        .env("LIB", lib_path())
        .env("GATE", &gate)
        .env("MV_FPS_PREFLIGHT_PROBE_CMD", &probe)
        .env("MV_FPS_PREFLIGHT_REPROBE_SLEEP", "0")
        .env("FAKE_PROBE_OUT", probe_out)
        .env("FAKE_GATE_EXITS", gate_exits)
        .env("FAKE_GATE_COUNTER", &counter)
        .current_dir(manifest_dir())
        .output()
        .expect("run assert harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn pass_proceeds() {
    // Gate exit 0 (above floor) -> ok, the run proceeds.
    let (rc, out, err) = run_assert(AUDIT_LINE, "0");
    assert_eq!(
        rc, 0,
        "a PASS box must not abort:\nstdout={out}\nstderr={err}"
    );
    assert!(out.contains("PROCEEDED"), "PASS must proceed:\n{out}");
    assert!(out.contains("ok:"), "PASS must log ok:\n{out}");
}

#[test]
fn unknown_no_audit_line_proceeds() {
    // No audit line read (box down / pre-issue-771 build / ssh failed) -> UNKNOWN -> NOTE, proceed.
    let (rc, out, err) = run_assert("", "0");
    assert_eq!(
        rc, 0,
        "an unreadable box must NOT abort a CI gate:\nstderr={err}"
    );
    assert!(out.contains("PROCEEDED"), "UNKNOWN must proceed:\n{out}");
    assert!(
        err.contains("no multiview-audit line"),
        "UNKNOWN(no audit) must log a NOTE:\n{err}"
    );
}

#[test]
fn unknown_unclassifiable_gate_proceeds() {
    // Audit lines present but the gate cannot classify (exit 2) -> UNKNOWN -> NOTE, proceed
    // (a missing/broken gate binary must never false-abort the whole E2E).
    let (rc, out, err) = run_assert(AUDIT_LINE, "2");
    assert_eq!(
        rc, 0,
        "an unclassifiable gate must NOT abort:\nstderr={err}"
    );
    assert!(
        out.contains("PROCEEDED"),
        "UNKNOWN(gate) must proceed:\n{out}"
    );
    assert!(
        err.contains("could not classify"),
        "UNKNOWN(gate) must name the classify failure:\n{err}"
    );
}

#[test]
fn a_single_transient_below_recovers_on_grace_reread_and_proceeds() {
    // Below floor on the FIRST read, back above floor on the grace re-read (a momentary contention
    // spike) -> must NOT abort. This is the "never false-abort a CI gate" guard.
    let (rc, out, err) = run_assert(AUDIT_LINE, "1 0");
    assert_eq!(
        rc, 0,
        "a transient below-floor read that recovers must NOT abort:\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("PROCEEDED"),
        "a recovered transient must proceed:\n{out}"
    );
    assert!(
        err.contains("grace re-read") && err.contains("recovered"),
        "the transient path must log the grace re-read + recovery:\n{err}"
    );
}

#[test]
fn a_sustained_below_floor_collapse_aborts_the_run() {
    // Below floor on BOTH the first read and the grace re-read (a real sustained collapse) -> exit 1,
    // loud ERROR, the run does NOT start.
    let (rc, out, err) = run_assert(AUDIT_LINE, "1 1");
    assert_eq!(
        rc, 1,
        "a CONFIRMED collapse must abort (exit 1):\nstdout={out}\nstderr={err}"
    );
    assert!(
        !out.contains("PROCEEDED"),
        "the run must NOT proceed past a confirmed collapse:\n{out}"
    );
    assert!(
        err.contains("CONFIRMED below its floor"),
        "the abort must name the confirmed below-floor collapse:\n{err}"
    );
    assert!(
        err.contains("strih"),
        "the abort must name the collapsed box:\n{err}"
    );
}
