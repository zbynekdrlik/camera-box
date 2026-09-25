//! Issue 1302 — the CG_CHAIN=1 cg OBS recording is decoded IN PLACE on RESOLUME-SNV (never copied
//! to dev1), concurrently with the strih/stream extracts, so the profile fits the full-path E2E job
//! budget. Covers:
//!   - scripts/lib/cg-chain-e2e.sh: the launch / collect / marker / merge-args seam, driven against a
//!     FAKE `recording-verdict-on-resolume.sh` in a temp scripts dir (no rig, no network);
//!   - scripts/recording-verdict-on-resolume.sh: the pure PowerShell builders, and its whole
//!     `main()` against fake `sshpass` / `ssh` / `scp` on PATH (the sha256 version gate, the on-box
//!     `--extract-partial cg` command, the partial pull-back);
//!   - scripts/recording-e2e.sh: the wiring order (static anchors).
//!
//! Tier-0: every test is a bash snippet run under the caller's real `set -euo pipefail`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/cg-chain-e2e.sh")
}

fn resolume_script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-resolume.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Run `snippet` under `set -euo pipefail` after sourcing `lib`. Returns (exit_ok, stdout, stderr).
fn run_sourced(lib: &Path, snippet: &str) -> (bool, String, String) {
    let script = format!("set -euo pipefail\n. \"{}\"\n{}", lib.display(), snippet);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn run(snippet: &str) -> (bool, String, String) {
    run_sourced(&lib_script(), snippet)
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).expect("write fake");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake");
}

/// A temp "scripts dir" holding a FAKE recording-verdict-on-resolume.sh. `mode`:
///   `ok`   — logs its argv + RESOLUME_BOX, writes the partial into --local-out-dir, exits 0;
///   `fail` — exits 5 without a partial;
///   `hang` — sleeps (the grace-overrun case).
fn fake_scripts_dir(mode: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${{1:-}}" = --stop-decode ]; then echo "STOP-DECODE BOX=$RESOLUME_BOX" >> "$(dirname "$0")/calls.log"; exit 0; fi
printf 'BOX=%s\n' "$RESOLUME_BOX" >> "$(dirname "$0")/calls.log"
printf 'ARG=%s\n' "$@" >> "$(dirname "$0")/calls.log"
out_dir=""; out=""; prev=""
for a in "$@"; do
  case "$prev" in --local-out-dir) out_dir="$a" ;; --out) out="$a" ;; esac
  prev="$a"
done
case "{mode}" in
  ok) base="${{out##*\\}}"; echo '{{}}' > "$out_dir/$base"; echo "fake extract done" ;;
  fail) echo "fake extract failed" >&2; exit 5 ;;
  hang) sleep 30 ;;
esac
"#
    );
    write_exec(&dir.path().join("recording-verdict-on-resolume.sh"), &body);
    dir
}

// ---- pure builders ------------------------------------------------------------------------------

#[test]
fn partial_log_and_onbox_paths_are_keyed_to_the_run() {
    let (ok, out, err) = run("CG_CHAIN_STATE_DIR=/r RUN_ID=42\n\
         printf '%s|%s|%s' \"$(cg_chain_partial_file)\" \"$(cg_chain_extract_log)\" \
         \"$(cg_chain_onbox_partial_win)\"");
    assert!(ok, "{err}");
    assert_eq!(
        out,
        r"/r/cg-partial-42.json|/r/cg-extract-42.log|C:\camera-box\verdict-out\cg-partial-42.json"
    );
    // An out-dir override moves the partial AND the dir the launch passes as --out-dir together.
    let (ok, out, err) = run("CG_CHAIN_ONBOX_OUT_DIR='D:\\cg' RUN_ID=42\n\
         printf '%s|%s' \"$(cg_chain_onbox_out_dir_win)\" \"$(cg_chain_onbox_partial_win)\"");
    assert!(ok, "{err}");
    assert_eq!(out, r"D:\cg|D:\cg\cg-partial-42.json");
}

#[test]
fn credentials_follow_the_env() {
    let (ok, out, err) = run(
        "CG_CHAIN_USER=x CG_CHAIN_PW=y; printf '%s/%s' \"$(cg_chain_user)\" \"$(cg_chain_pw)\"",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "x/y");
}

#[test]
fn grace_defaults_and_rejects_garbage() {
    for (val, want) in [
        ("", "300"),
        ("abc", "300"),
        ("-3", "300"),
        ("0", "0"),
        ("45", "45"),
    ] {
        let (ok, out, err) = run(&format!(
            "CG_CHAIN_EXTRACT_GRACE_SECS='{val}'; cg_chain_extract_grace_secs"
        ));
        assert!(ok, "{err}");
        assert_eq!(out, want, "{val:?}");
    }
}

#[test]
fn leg_marker_names_all_three_outcomes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let partial = dir.path().join("cg-partial-1.json");
    fs::write(&partial, "{}").expect("write partial");
    let p = partial.display();
    let (ok, out, err) = run(&format!(
        "cg_chain_leg_marker '{p}' '' ''\n\
         cg_chain_leg_marker '{p}' skipped 'resolume away'\n\
         cg_chain_leg_marker /nope/x.json failed 'decode failed'\n\
         cg_chain_leg_marker /nope/x.json '' ''\n\
         cg_chain_leg_marker '{p}' failed 'stopped'"
    ));
    assert!(ok, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 5, "{out}");
    // VERIFIED claims only what is true at collect time: the partial reached dev1 (the merge can
    // still drop it, with its own warning).
    assert!(lines[0].starts_with("CG-LEG-VERIFIED:"), "{}", lines[0]);
    assert!(lines[0].contains("reached dev1"), "{}", lines[0]);
    assert!(
        lines[1].starts_with("CG-LEG-SKIPPED: resolume away"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("CG-LEG-NOT-VERIFIED: decode failed"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].starts_with("CG-LEG-NOT-VERIFIED: no cg partial reached dev1"),
        "{}",
        lines[3]
    );
    // A failed state never reads as verified, even if a file happens to exist.
    assert!(
        lines[4].starts_with("CG-LEG-NOT-VERIFIED: stopped"),
        "{}",
        lines[4]
    );
    for l in &lines[1..] {
        assert!(l.contains("camera-chain gate is unaffected"), "{l}");
    }
}

// ---- launch + collect ---------------------------------------------------------------------------

#[test]
fn disabled_profile_launches_nothing_and_prints_nothing() {
    let scripts = fake_scripts_dir("ok");
    let (ok, out, err) = run(&format!(
        "unset CG_CHAIN; CG_RECORDING_STARTED=1 CG_HOST_IP=1.2.3.4 CG_HOST_RECORDING_PATH='C:\\x.mkv'\n\
         cg_chain_onbox_extract_launch '{}' /bin/true 1\n\
         cg_chain_onbox_extract_wait\n\
         printf 'PID=[%s]' \"${{CG_EXTRACT_PID:-}}\"",
        scripts.path().display()
    ));
    assert!(ok, "{err}");
    assert_eq!(out, "PID=[]");
    assert!(!scripts.path().join("calls.log").exists());
}

#[test]
fn a_cg_recording_that_never_started_is_an_explicit_skip_never_a_red() {
    let scripts = fake_scripts_dir("ok");
    let state = tempfile::tempdir().expect("tempdir");
    // A stale partial of this run id is removed at launch, so it can never be merged.
    fs::write(state.path().join("cg-partial-9.json"), "{}").expect("stale partial");
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_RECORDING_STARTED=0 CG_CHAIN_STATE_DIR='{}' RUN_ID=9\n\
         cg_chain_onbox_extract_launch '{}' /bin/true 1\n\
         cg_chain_onbox_extract_wait\n\
         MERGE_ARGS=(a); cg_chain_merge_args_append; printf 'ARGS=%s|' \"${{MERGE_ARGS[@]}}\"",
        state.path().display(),
        scripts.path().display()
    ));
    assert!(ok, "{err}");
    assert!(
        out.contains("CG-LEG-SKIPPED: no cg OBS recording this run"),
        "{out}"
    );
    assert!(out.ends_with("ARGS=a|ARGS=--cg-chain-burns|"), "{out}");
    assert!(!state.path().join("cg-partial-9.json").exists());
    assert!(!scripts.path().join("calls.log").exists());
}

#[test]
fn a_plan_only_run_skips_and_a_missing_exe_does_not_verify() {
    let scripts = fake_scripts_dir("ok");
    let state = tempfile::tempdir().expect("tempdir");
    let base = format!(
        "CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_HOST_IP=1.2.3.4 CG_HOST_RECORDING_PATH='C:\\x.mkv' \
         CG_CHAIN_STATE_DIR='{}' RUN_ID=3\n",
        state.path().display()
    );
    let (ok, out, err) = run(&format!(
        "{base}cg_chain_onbox_extract_launch '{}' /bin/true 0\ncg_chain_onbox_extract_wait",
        scripts.path().display()
    ));
    assert!(ok, "{err}");
    assert!(out.starts_with("CG-LEG-SKIPPED: plan-only run"), "{out}");
    let (ok, out, err) = run(&format!(
        "{base}cg_chain_onbox_extract_launch '{}' /nope/recording-verdict.exe 1\n\
         cg_chain_onbox_extract_wait",
        scripts.path().display()
    ));
    assert!(ok, "{err}");
    assert!(
        out.starts_with("CG-LEG-NOT-VERIFIED: no Windows recording-verdict.exe"),
        "{out}"
    );
    assert!(!scripts.path().join("calls.log").exists());
}

#[test]
fn a_launched_extract_is_collected_and_its_partial_is_merged() {
    let scripts = fake_scripts_dir("ok");
    let state = tempfile::tempdir().expect("tempdir");
    let exe = state.path().join("recording-verdict.exe");
    fs::write(&exe, "exe").expect("exe");
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_HOST_IP=10.77.9.201 \
         CG_HOST_RECORDING_PATH='C:/Users/Resolume/Videos/2026-09-25 11-26-55.mkv' \
         CG_CHAIN_STATE_DIR='{d}' RUN_ID=5 CG_CHAIN_EXTRACT_POLL_SECS=1\n\
         cg_chain_onbox_extract_launch '{s}' '{e}' 1\n\
         [ -n \"$CG_EXTRACT_PID\" ] && echo LAUNCHED\n\
         cg_chain_onbox_extract_wait\n\
         MERGE_ARGS=(a); cg_chain_merge_args_append; printf 'ARGS=%s|' \"${{MERGE_ARGS[@]}}\"",
        d = state.path().display(),
        s = scripts.path().display(),
        e = exe.display()
    ));
    assert!(ok, "{err}");
    assert!(out.contains("LAUNCHED"), "{out}");
    assert!(
        out.contains("fake extract done"),
        "the extract log is replayed: {out}"
    );
    assert!(out.contains("CG-LEG-VERIFIED:"), "{out}");
    assert!(
        out.contains("cg extract collected "),
        "when the extract was collected is logged (the on-box decode time is its STEP 2 line): {out}"
    );
    let partial = state.path().join("cg-partial-5.json");
    assert!(
        out.ends_with(&format!(
            "ARGS=a|ARGS=--cg-chain-burns|ARGS=--merge-partials|ARGS=cg={}|",
            partial.display()
        )),
        "{out}"
    );
    let calls = fs::read_to_string(scripts.path().join("calls.log")).expect("calls");
    assert!(calls.contains("BOX=10.77.9.201"), "{calls}");
    for want in [
        "ARG=--extract-partial",
        "ARG=cg",
        "ARG=--cg",
        "ARG=C:/Users/Resolume/Videos/2026-09-25 11-26-55.mkv",
        r"ARG=C:\camera-box\verdict-out\cg-partial-5.json",
        "ARG=--out-dir",
        r"ARG=C:\camera-box\verdict-out",
        &format!("ARG={}", exe.display()),
    ] {
        assert!(calls.lines().any(|l| l == want), "missing {want}: {calls}");
    }
    assert!(
        !calls.contains("STOP-DECODE"),
        "a finished decode is never stopped: {calls}"
    );
}

#[test]
fn a_failed_extract_is_not_verified_and_leaves_no_partial() {
    let scripts = fake_scripts_dir("fail");
    let state = tempfile::tempdir().expect("tempdir");
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_HOST_IP=1.2.3.4 CG_HOST_RECORDING_PATH='C:\\x.mkv' \
         CG_CHAIN_STATE_DIR='{d}' RUN_ID=6 CG_CHAIN_EXTRACT_POLL_SECS=1\n\
         cg_chain_onbox_extract_launch '{s}' /bin/true 1\n\
         cg_chain_onbox_extract_wait\n\
         MERGE_ARGS=(a); cg_chain_merge_args_append; printf 'ARGS=%s|' \"${{MERGE_ARGS[@]}}\"",
        d = state.path().display(),
        s = scripts.path().display()
    ));
    assert!(ok, "the report-only leg never aborts the harness: {err}");
    assert!(out.contains("CG-LEG-NOT-VERIFIED:"), "{out}");
    assert!(out.contains("rc=5"), "{out}");
    assert!(out.ends_with("ARGS=a|ARGS=--cg-chain-burns|"), "{out}");
    // A dev1 side that died (ssh drop) may leave the decode running on the box: stop it too.
    let calls = fs::read_to_string(scripts.path().join("calls.log")).expect("calls");
    assert!(calls.contains("STOP-DECODE BOX=1.2.3.4"), "{calls}");
}

#[test]
fn an_extract_past_its_grace_is_stopped_and_never_holds_the_job() {
    let scripts = fake_scripts_dir("hang");
    let state = tempfile::tempdir().expect("tempdir");
    let started = std::time::Instant::now();
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_HOST_IP=1.2.3.4 CG_HOST_RECORDING_PATH='C:\\x.mkv' \
         CG_CHAIN_STATE_DIR='{d}' RUN_ID=7 CG_CHAIN_EXTRACT_GRACE_SECS=1 CG_CHAIN_EXTRACT_POLL_SECS=1\n\
         cg_chain_onbox_extract_launch '{s}' /bin/true 1\n\
         pid=\"$CG_EXTRACT_PID\"\n\
         cg_chain_onbox_extract_wait\n\
         if kill -0 \"$pid\" 2>/dev/null; then echo STILL-RUNNING; else echo GONE; fi",
        d = state.path().display(),
        s = scripts.path().display()
    ));
    assert!(ok, "{err}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the grace bounds the wait"
    );
    assert!(out.contains("CG-LEG-NOT-VERIFIED:"), "{out}");
    assert!(
        out.contains("still running after "),
        "the elapsed time is in the reason: {out}"
    );
    assert!(out.ends_with("GONE"), "{out}");
    // The decode it started ON the box is asked to stop too (never left next to Arena / cg OBS).
    let calls = fs::read_to_string(scripts.path().join("calls.log")).expect("calls");
    assert!(calls.contains("STOP-DECODE BOX=1.2.3.4"), "{calls}");
    assert_eq!(
        calls.matches("STOP-DECODE").count(),
        1,
        "stopped once: {calls}"
    );
}

#[test]
fn cleanup_stops_an_inflight_extract() {
    let scripts = fake_scripts_dir("hang");
    let state = tempfile::tempdir().expect("tempdir");
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_HOST_IP=1.2.3.4 CG_HOST_RECORDING_PATH='C:\\x.mkv' \
         CG_CHAIN_STATE_DIR='{d}' RUN_ID=8 CG_CHAIN_SONGPLAYER_API=http://127.0.0.1:1 \
         CG_CHAIN_CLEANUP_BURN_TIMEOUT=1 CG_CHAIN_BURN_ATTEMPTS=1\n\
         cg_chain_onbox_extract_launch '{s}' /bin/true 1\n\
         pid=\"$CG_EXTRACT_PID\"\n\
         CG_RECORDING_STARTED=0 cg_chain_cleanup '' /nope/obs_phase2.py 1\n\
         wait \"$pid\" 2>/dev/null || true\n\
         if kill -0 \"$pid\" 2>/dev/null; then echo STILL-RUNNING; else echo GONE; fi",
        d = state.path().display(),
        s = scripts.path().display()
    ));
    assert!(ok, "{err}");
    assert!(
        err.contains("stopped the in-flight cg OBS extract"),
        "{err}"
    );
    assert!(out.ends_with("GONE"), "{out}");
    let calls = fs::read_to_string(scripts.path().join("calls.log")).expect("calls");
    assert!(calls.contains("STOP-DECODE BOX=1.2.3.4"), "{calls}");
}

// ---- recording-verdict-on-resolume.sh -----------------------------------------------------------

fn resolume(snippet: &str) -> (bool, String, String) {
    run_sourced(&resolume_script(), snippet)
}

#[test]
fn resolume_builders_are_well_formed_powershell() {
    let (ok, out, err) = resolume(
        "onresolume_sha_probe_ps 'C:\\camera-box\\recording-verdict.exe'; echo\n\
         onresolume_tool_preflight_ps; echo\n\
         onresolume_prepare_ps 'C:\\camera-box\\verdict-out' 'C:\\camera-box\\verdict-out\\cg-partial-1.json' \
           'C:\\camera-box\\recording-verdict.exe'; echo\n\
         onresolume_ffmpeg_path_ps 'C:\\ffmpeg'; echo\n\
         printf '[%s]\\n' \"$(onresolume_ffmpeg_path_ps '')\"\n\
         onresolume_path_exists_ps 'C:\\camera-box\\verdict-out\\cg-partial-1-pixels'; echo\n\
         onresolume_ps_quote \"$(printf 'a%sb%sc%sd' \"'\" '$' '`')\"; echo",
    );
    assert!(ok, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        r#"if (Test-Path -LiteralPath 'C:\camera-box\recording-verdict.exe' -PathType Leaf) { (Get-FileHash -Algorithm SHA256 -LiteralPath 'C:\camera-box\recording-verdict.exe').Hash.ToLower() }"#
    );
    assert!(
        lines[1].contains(r#"@("ffmpeg","ffprobe")"#),
        "{}",
        lines[1]
    );
    assert!(lines[1].contains("exit 3"), "{}", lines[1]);
    assert!(lines[1].contains("MISSING-TOOL"), "{}", lines[1]);
    // The prep stops only a leftover decode of THIS exe, then deletes guarded — nothing to delete
    // must be a SUCCESS: PowerShell exits 1 when the last statement failed, even when its error
    // is silenced (the first draft ended in `Remove-Item … -ErrorAction SilentlyContinue`).
    assert_eq!(
        lines[2],
        r#"Get-Process -Name 'recording-verdict' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq 'C:\camera-box\recording-verdict.exe' } | Stop-Process -Force; New-Item -ItemType Directory -Force -Path 'C:\camera-box\verdict-out' | Out-Null; foreach ($p in @('C:\camera-box\verdict-out\cg-partial-1.json', 'C:\camera-box\verdict-out\cg-partial-1-pixels')) { if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force } }"#
    );
    // ffmpeg is found under the root (RESOLUME-SNV keeps it off PATH), newest build first, and put
    // first on PATH; the trailing "; " lets it prefix the next statement.
    assert_eq!(
        lines[3],
        r#"$f = Get-ChildItem -LiteralPath 'C:\ffmpeg' -Filter ffmpeg.exe -Recurse -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1; if ($f) { $env:Path = $f.DirectoryName + ";" + $env:Path }; "#
    );
    assert_eq!(lines[4], "[]", "an empty root adds no prefix");
    // The pixel-dir probe is PowerShell, so it works whatever the box's OpenSSH default shell is.
    assert_eq!(
        lines[5],
        r#"if (Test-Path -LiteralPath 'C:\camera-box\verdict-out\cg-partial-1-pixels') { exit 0 } else { exit 1 }"#
    );
    // Paths are single-quoted PowerShell literals: `$` and a backtick stay literal, `'` doubles.
    assert_eq!(lines[6], "'a''b$c`d'");
}

/// Fake `sshpass` / `ssh` / `scp` on PATH: ssh decodes each `-EncodedCommand` into `ssh.log` and
/// answers the sha256 probe with `$FAKE_REMOTE_SHA`; the Test-Path pixel-dir probe says absent;
/// scp logs to `scp.log` and materialises a downloaded file. It models PowerShell's exit code on a
/// box where nothing is there to delete or stop: a text whose LAST statement is a silenced
/// cmdlet (`… -ErrorAction SilentlyContinue`) fails, exactly as `powershell -EncodedCommand` does.
const FAKE_WIN_SSH: &str = r#"
F="$(mktemp -d)"; trap 'rm -rf "$F"' EXIT
cat > "$F/sshpass" <<'SH'
#!/usr/bin/env bash
shift 2; exec "$@"
SH
cat > "$F/ssh" <<'SH'
#!/usr/bin/env bash
for last in "$@"; do :; done
case "$last" in
  "if exist "*) exit 1 ;;
  powershell*)
    dec="$(printf '%s' "${last##* }" | base64 -d | iconv -f UTF-16LE -t UTF-8)"
    printf '%s\n' "$dec" >> "$FAKE_LOG_DIR/ssh.log"
    case "$dec" in *Get-FileHash*) printf '%s\r\n' "${FAKE_REMOTE_SHA:-}" ;; esac
    case "$dec" in *"-pixels') { exit 0 } else { exit 1 }") exit "${FAKE_PIXEL_RC:-1}" ;; esac
    case "$dec" in *SilentlyContinue) exit 1 ;; esac ;;
esac
exit 0
SH
cat > "$F/scp" <<'SH'
#!/usr/bin/env bash
n=$#; src="${@:n-1:1}"; dst="${@:n:1}"
printf '%s -> %s\n' "$src" "$dst" >> "$FAKE_LOG_DIR/scp.log"
case "$src" in *@*) echo '{}' > "$dst" ;; esac
SH
chmod +x "$F/sshpass" "$F/ssh" "$F/scp"; PATH="$F:$PATH"
"#;

/// What one `recording-verdict-on-resolume.sh` run against the fakes produced.
struct ResolumeRun {
    ok: bool,
    stdout: String,
    stderr: String,
    ssh_log: String,
    scp_log: String,
    partial_pulled: bool,
}

fn run_resolume_main(remote_sha_same: bool, pixel_probe_rc: u8) -> ResolumeRun {
    let logs = tempfile::tempdir().expect("tempdir");
    let exe = logs.path().join("recording-verdict.exe");
    fs::write(&exe, "the-exe-bytes").expect("exe");
    let out_dir = logs.path().join("pulled");
    let sha = if remote_sha_same {
        "\"$(sha256sum \"$EXE\" | cut -d' ' -f1)\"".to_string()
    } else {
        "0000".to_string()
    };
    let script = format!(
        "set -euo pipefail\n{FAKE_WIN_SSH}\nexport FAKE_LOG_DIR='{l}' EXE='{e}'\n\
         export FAKE_REMOTE_SHA={sha} FAKE_PIXEL_RC={pixel_probe_rc}\n\
         RESOLUME_BOX=10.77.9.201 RESOLUME_USER=u RESOLUME_PW=p '{r}' \
         --verdict-exe-local '{e}' --local-out-dir '{o}' \
         -- --extract-partial cg --cg 'C:/Users/Resolume/Videos/a b.mkv' \
         --out 'C:\\camera-box\\verdict-out\\cg-partial-4.json'",
        l = logs.path().display(),
        e = exe.display(),
        r = resolume_script().display(),
        o = out_dir.display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    // The logs are read back as strings before the tempdir is dropped here.
    ResolumeRun {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        ssh_log: fs::read_to_string(logs.path().join("ssh.log")).unwrap_or_default(),
        scp_log: fs::read_to_string(logs.path().join("scp.log")).unwrap_or_default(),
        partial_pulled: out_dir.join("cg-partial-4.json").exists(),
    }
}

#[test]
fn resolume_main_decodes_on_the_box_and_pulls_back_only_the_partial() {
    let r = run_resolume_main(false, 1);
    let (ssh_log, scp_log) = (&r.ssh_log, &r.scp_log);
    assert!(r.ok, "{}\n{}", r.stdout, r.stderr);
    // STEP 0 preflight, the stale-output prep, the sha probe, then the decode itself.
    let pre = ssh_log.find("MISSING-TOOL").expect("preflight ran");
    let prep = ssh_log.find("Remove-Item").expect("prep ran");
    let sha = ssh_log.find("Get-FileHash").expect("sha probe ran");
    let dec = ssh_log
        .find(r#""--extract-partial" "cg" "--cg" "C:/Users/Resolume/Videos/a b.mkv""#)
        .expect("the on-box cg decode ran");
    assert!(pre < prep && prep < sha && sha < dec, "{ssh_log}");
    // ffmpeg is put on PATH in BOTH sessions that need it (preflight and decode).
    let ffmpeg = r#"Get-ChildItem -LiteralPath 'C:\ffmpeg' -Filter ffmpeg.exe"#;
    assert_eq!(ssh_log.matches(ffmpeg).count(), 2, "{ssh_log}");
    let dec_line = ssh_log
        .lines()
        .find(|l| l.contains(r#""--extract-partial" "cg""#))
        .expect("decode line");
    assert!(
        dec_line.starts_with("$f = Get-ChildItem"),
        "the decode session finds ffmpeg first: {dec_line}"
    );
    assert!(
        ssh_log.contains(r#"PriorityClass = "BelowNormal""#),
        "the decode yields to the live obs64/Arena: {ssh_log}"
    );
    // A differing on-box sha re-uploads the exe (the issue-1118 version gate).
    assert!(
        scp_log.contains(r"u@10.77.9.201:C:\camera-box\recording-verdict.exe"),
        "{scp_log}"
    );
    // Only the partial comes back; the recording is never a scp source.
    assert!(
        scp_log.contains("u@10.77.9.201:C:/camera-box/verdict-out/cg-partial-4.json"),
        "{scp_log}"
    );
    assert!(!scp_log.contains(".mkv"), "{scp_log}");
    assert!(r.partial_pulled, "the partial landed in --local-out-dir");
    assert!(
        ssh_log.contains(
            r#"if (Test-Path -LiteralPath 'C:\camera-box\verdict-out\cg-partial-4-pixels') { exit 0 } else { exit 1 }"#
        ),
        "the pixel-dir probe runs through PowerShell: {ssh_log}"
    );
    assert!(r.stdout.contains("STEP 2 decode took "), "{}", r.stdout);
}

#[test]
fn resolume_main_skips_the_upload_for_an_identical_binary() {
    let r = run_resolume_main(true, 1);
    assert!(r.ok, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("upload skipped"), "{}", r.stdout);
    assert!(
        !r.scp_log.contains("recording-verdict.exe"),
        "{}",
        r.scp_log
    );
    assert!(r.partial_pulled);
}

#[test]
fn resolume_main_names_a_failed_pixel_probe_instead_of_nothing_flagged() {
    // A transport failure on the pixel-dir probe (ssh 255) is not "absent": say so, never claim
    // that nothing was flagged. The partial itself still comes back.
    let r = run_resolume_main(false, 255);
    assert!(r.ok, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("could not probe"), "{}", r.stderr);
    assert!(r.stderr.contains("rc=255"), "{}", r.stderr);
    assert!(!r.stdout.contains("nothing was flagged"), "{}", r.stdout);
    assert!(r.partial_pulled);
}

#[test]
fn resolume_stop_decode_stops_only_that_exe_and_never_fails() {
    let logs = tempfile::tempdir().expect("tempdir");
    let script = format!(
        "set -euo pipefail\n{FAKE_WIN_SSH}\nexport FAKE_LOG_DIR='{l}'\n\
         RESOLUME_BOX=10.77.9.201 RESOLUME_USER=u RESOLUME_PW=p '{r}' --stop-decode; echo RC=$?",
        l = logs.path().display(),
        r = resolume_script().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.trim_end().ends_with("RC=0"), "{stdout}");
    let ssh_log = fs::read_to_string(logs.path().join("ssh.log")).expect("ssh log");
    assert!(
        ssh_log.contains(r#"Where-Object { $_.Path -eq 'C:\camera-box\recording-verdict.exe' } | Stop-Process -Force; exit 0"#),
        "{ssh_log}"
    );
    assert!(
        !logs.path().join("scp.log").exists(),
        "a stop never copies anything"
    );
}

#[test]
fn resolume_main_refuses_without_credentials() {
    // main() exits, so it runs in a subshell and the snippet reports its code.
    let (ok, out, err) = resolume(
        "( RESOLUME_USER='' RESOLUME_PW='' main --verdict-exe-local /bin/true --local-out-dir /tmp \
         -- --out x.json ) || echo RC=$?",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "RC=2");
    assert!(err.contains("RESOLUME_USER / RESOLUME_PW"), "{err}");
}

// ---- recording-e2e.sh wiring ------------------------------------------------------------------

#[test]
fn recording_e2e_launches_the_cg_extract_next_to_the_stream_extract() {
    let s = recording_e2e_text();
    assert_eq!(
        s.matches("cg_chain_onbox_extract_launch \"$HERE\"").count(),
        1
    );
    let stream = s
        .find("run_stream_extract >\"$STREAM_EXTRACT_LOG\" 2>&1 &")
        .expect("the backgrounded stream extract");
    let launch = s
        .find("cg_chain_onbox_extract_launch \"$HERE\" \"${WIN_VERDICT_EXE_LOCAL:-}\" \"$E2E_EXECUTE_VERDICT\"")
        .expect("the cg extract launch");
    let imag = s
        .find("IMAG_BOX=\"$IMAG_IP\" \"$HERE/recording-verdict-on-imag.sh\"")
        .expect("the synchronous imag extract");
    assert!(
        stream < launch && launch < imag,
        "launched after stream and before the synchronous imag decode, so all run concurrently"
    );
}

#[test]
fn recording_e2e_collects_the_cg_extract_after_the_camera_legs_before_the_merge() {
    let s = recording_e2e_text();
    let camera_wait = s
        .find("wait \"$STREAM_EXTRACT_PID\"")
        .expect("the stream extract wait");
    let collect = s
        .find("\n  cg_chain_onbox_extract_wait\n")
        .expect("the cg collect step");
    let merge = s
        .find("--- [8/8d] MERGE the small partials ON dev1")
        .expect("the merge banner");
    let append = s
        .find("\n  cg_chain_merge_args_append\n")
        .expect("the cg merge args");
    assert!(camera_wait < collect && collect < merge && merge < append);
}

#[test]
fn recording_e2e_never_copies_the_cg_recording_to_dev1() {
    let s = recording_e2e_text();
    for gone in [
        "cg_chain_pull_recording",
        "CG_RECORDING=",
        "--cg \"$CG_RECORDING\"",
    ] {
        assert!(!s.contains(gone), "{gone} must stay gone");
    }
    assert!(
        s.contains("CG_EXTRACT_PID=\"\""),
        "initialised before the trap"
    );
}
