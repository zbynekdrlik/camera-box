//! issue 1360 — ONE platform-resolved strih OBS-log reader (`scripts/lib/strih-log-read.sh`)
//! shared by the helpers that read the strih OBS log: `qr-align.sh` (the #1161 floor-aware
//! arrival audit), `genlock-settle.sh` (`[4j/8settle]`), `mv-reverify-escalate.sh` →
//! `frozen-cam-received.sh` (`[4c/8]` received= liveness), `ndi-cadence-heal.sh` (the cleanup
//! cadence verify), and the MV-fps pair (`mv-fps-preflight.sh` + `mv-fps-alert-watchdog.sh`, the
//! reader side of `mv-fps-health.sh`).
//!
//! Root cause: each helper grew its own Windows-only reader (PowerShell `Get-Content`/`gc` of the
//! newest `$env:APPDATA\obs-studio\logs\*.txt`) before the M4 cut-over moved the strih role to
//! the Linux notebook strih-lx (10.77.9.202), so on strih-lx every read came back empty and each
//! consumer fell back fail-open (`READ_FAIL`, `ssh flake/timeout`, settle never quiet).
//!
//! All Tier-0 (no rig, no real ssh): each case is a bash FILE run as `bash <case.sh> <tmpdir>`
//! under the callers' own `set -euo pipefail` (the #1133 bare-statement class). PATH-stubbed
//! `sshpass` logs its argv and execs the rest (`timeout T ssh …`); the PATH-stubbed `ssh` runs a
//! LINUX remote command for real against a fixture HOME whose newest OBS log has SPACES in its
//! name (`2026-09-23 09-23-36.txt`, the strih-lx naming) and answers a `powershell …` one with a
//! canned Windows reply — so the platform branch actually taken is observable end-to-end.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The common bash prelude: stubs + the fixture HOME. `$1` = the per-case temp dir. Run with the
/// repo root as cwd, so `$R` (= `$PWD`) is the checkout.
const PRELUDE: &str = r#"set -euo pipefail
T="$1"
shift
R="$PWD"
unset STRIH_PLATFORM STRIH_LX_HOST MV_REVERIFY_RECEIVED_CMD FROZEN_CAM_RECEIVED_CMD \
  NDI_CADENCE_FETCH_CMD GENLOCK_SETTLE_READER_CMD MV_FPS_PREFLIGHT_PROBE_CMD MV_FPS_PROBE_CMD \
  QR_ALIGN_JITTER_JSON STUB_SSH_FAIL STUB_WIN_OUT 2>/dev/null || true
mkdir -p "$T/bin" "$T/home/.config/obs-studio/logs"
cat > "$T/bin/sshpass" <<'STUB'
#!/usr/bin/env bash
printf 'SSHPASS:%s\n' "$*" >> "$STUB_LOG"
[ "${STUB_SSH_FAIL:-0}" = 1 ] && exit 255
shift 2
exec "$@"
STUB
cat > "$T/bin/ssh" <<'STUB'
#!/usr/bin/env bash
for a in "$@"; do last="$a"; done
printf 'REMOTE:%s\n' "$last" >> "$STUB_LOG"
case "$last" in
  powershell*) printf '%s\n' "${STUB_WIN_OUT:-win-reply}" ;;
  *) HOME="$FAKE_HOME" exec bash -c "$last" ;;
esac
STUB
chmod +x "$T/bin/sshpass" "$T/bin/ssh"
install_py_stub() {
  cat > "$T/bin/python3" <<'STUB'
#!/usr/bin/env bash
printf 'PY:%s\n' "$*" >> "$STUB_LOG"
exit 0
STUB
  chmod +x "$T/bin/python3"
}
export PATH="$T/bin:$PATH" STUB_LOG="$T/stub.log" FAKE_HOME="$T/home"
: > "$STUB_LOG"
L="$T/home/.config/obs-studio/logs"
printf 'OLD-LOG-LINE\n' > "$L/2026-09-22 08-00-00.txt"
{
  printf '%s\n' "09:23:36.001: genlock-fifo audit 'NDI cam1': received=100 relocks=0 underruns=0 dropped_due=0 late_holds=0"
  printf '%s\n' "09:23:37.002: multiview-audit: monitor=1 divisor=1 rendered_fps=29.97 target=30 floor=28 cx=1920 cy=1080"
  printf '%s\n' "09:23:38.003: recv-timing #797 'NDI cam1': n=300 cap_avg=16.67ms"
  printf '%s\n' "09:23:39.004: genlock-fifo audit 'NDI cam1': received=160 relocks=0 underruns=0 dropped_due=0 late_holds=0"
  printf '%s\n' "09:23:40.005: newest-line-5"
} > "$L/2026-09-23 09-23-36.txt"
touch -d '2026-09-22 08:00:00' "$L/2026-09-22 08-00-00.txt"
touch -d '2026-09-23 09:23:40' "$L/2026-09-23 09-23-36.txt"
"#;

struct CaseOut {
    code: i32,
    stdout: String,
    stderr: String,
    log: String,
}

/// Write PRELUDE + `body` + a sentinel to `<tmp>/case.sh` and run it as `bash <file> <tmp>`.
fn run_case(body: &str) -> CaseOut {
    let dir = tempfile::tempdir().expect("tempdir");
    let t = dir.path();
    let case = t.join("case.sh");
    fs::write(&case, format!("{PRELUDE}\n{body}\necho __CASE_DONE__\n")).expect("write case");
    let out = Command::new("bash")
        .arg(&case)
        .arg(t)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash case");
    let log = fs::read_to_string(t.join("stub.log")).unwrap_or_default();
    CaseOut {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        log,
    }
}

fn assert_done(o: &CaseOut, ctx: &str) {
    assert!(
        o.code == 0 && o.stdout.contains("__CASE_DONE__"),
        "{ctx}: the case must reach its sentinel under set -euo pipefail (rc={}).\nstdout:\n{}\nstderr:\n{}\nstub log:\n{}",
        o.code,
        o.stdout,
        o.stderr,
        o.log
    );
}

fn has_line(text: &str, want: &str) -> bool {
    text.lines().any(|l| l == want)
}

const LIB: &str = r#". "$R/scripts/lib/strih-log-read.sh""#;

// ---------------------------------------------------------------------------------------------
// (a) the shared lib itself
// ---------------------------------------------------------------------------------------------

#[test]
fn strih_log_os_resolves_linux_for_strih_lx_and_win_otherwise_1360() {
    let o = run_case(&format!(
        "{LIB}\na=\"$(strih_log_os 10.77.9.202)\"\nb=\"$(strih_log_os 10.77.9.99)\"\n\
         c=\"$(STRIH_PLATFORM=linux strih_log_os 10.0.0.1)\"\necho \"OS:$a,$b,$c\""
    ));
    assert_done(&o, "strih_log_os");
    assert!(
        has_line(&o.stdout, "OS:linux,win,linux"),
        "strih_log_os must map strih_platform onto the os-token vocabulary (linux|win): {}",
        o.stdout
    );
}

#[test]
fn linux_tail_reads_the_newest_log_with_spaces_in_its_name_1360() {
    let o = run_case(&format!(
        "{LIB}\necho 'OUT<<'\nstrih_log_tail 10.77.9.202 newlevel pw 2\necho '>>OUT'"
    ));
    assert_done(&o, "linux tail");
    assert!(
        o.stdout.contains("received=160") && o.stdout.contains("newest-line-5"),
        "the Linux tail must return the NEWEST log's last lines (a spaced filename): {}",
        o.stdout
    );
    assert!(
        !o.stdout.contains("OLD-LOG-LINE") && !o.stdout.contains("recv-timing"),
        "the Linux tail must read only the newest log's last 2 lines: {}",
        o.stdout
    );
    assert!(
        !o.log.contains("EncodedCommand"),
        "a Linux strih must never be read the PowerShell way: {}",
        o.log
    );
    assert!(
        o.log.contains("SSHPASS:-p pw timeout "),
        "`timeout` must sit INSIDE sshpass (the stub-bypass rule): {}",
        o.log
    );
}

#[test]
fn linux_line_count_and_since_line_scope_to_the_post_mark_lines_1360() {
    let o = run_case(&format!(
        "{LIB}\necho \"COUNT=$(strih_log_line_count 10.77.9.202 newlevel pw)\"\n\
         echo 'SINCE<<'\nstrih_log_since_line 10.77.9.202 newlevel pw 3\necho '>>SINCE'"
    ));
    assert_done(&o, "linux count/since");
    assert!(
        has_line(&o.stdout, "COUNT=5"),
        "the Linux line count must be the newest log's line count: {}",
        o.stdout
    );
    let since = o
        .stdout
        .split("SINCE<<")
        .nth(1)
        .and_then(|s| s.split(">>SINCE").next())
        .unwrap_or("");
    assert!(
        since.contains("received=160") && since.contains("newest-line-5"),
        "since-line 3 must return lines 4..5: {since}"
    );
    assert!(
        !since.contains("recv-timing") && !since.contains("received=100"),
        "since-line 3 must skip the first 3 lines (no regime-mixed whole-log fetch): {since}"
    );
}

#[test]
fn windows_commands_are_the_existing_powershell_strings_verbatim_1360() {
    let o = run_case(&format!(
        "{LIB}\ndec() {{ printf '%s' \"${{1##*-EncodedCommand }}\" | base64 -d | iconv -f UTF-16LE -t UTF-8; }}\n\
         t=\"$(strih_log_remote_cmd windows tail 400)\"\necho \"TAILHEAD=${{t%%-EncodedCommand*}}|\"\n\
         echo \"TAIL=$(dec \"$t\")\"\n\
         echo \"COUNT=$(dec \"$(strih_log_remote_cmd windows count)\")\"\n\
         echo \"SINCE=$(dec \"$(strih_log_remote_cmd windows since 7)\")\""
    ));
    assert_done(&o, "windows builders");
    assert!(
        has_line(
            &o.stdout,
            "TAILHEAD=powershell -NoProfile -NonInteractive |"
        ),
        "the Windows read must be a cmd.exe-proof -EncodedCommand powershell: {}",
        o.stdout
    );
    // The mv-reverify / ndi-cadence tail string, byte-for-byte.
    assert!(
        has_line(
            &o.stdout,
            r#"TAIL=gc (gci $env:APPDATA\obs-studio\logs\*.txt | sort LastWriteTime | select -last 1).FullName -Tail 400"#
        ),
        "the Windows tail must be the existing gc/gci -Tail string verbatim: {}",
        o.stdout
    );
    // The qr-align count + since strings, byte-for-byte.
    assert!(
        has_line(
            &o.stdout,
            r#"COUNT=(Get-Content (Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1)).Count"#
        ),
        "the Windows line count must be qr-align's existing string verbatim: {}",
        o.stdout
    );
    assert!(
        has_line(
            &o.stdout,
            r#"SINCE=Get-Content (Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1) | Select-Object -Skip 7"#
        ),
        "the Windows since-line must be qr-align's existing string verbatim: {}",
        o.stdout
    );
}

#[test]
fn windows_host_is_read_through_encoded_powershell_1360() {
    let o = run_case(&format!(
        "{LIB}\nexport STUB_WIN_OUT=WIN-TAIL-TEXT\necho \"OUT=$(strih_log_tail 10.0.0.1 newlevel pw 400)\""
    ));
    assert_done(&o, "windows transport");
    assert!(
        has_line(&o.stdout, "OUT=WIN-TAIL-TEXT"),
        "a Windows strih's tail must come back through the stubbed ssh: {}",
        o.stdout
    );
    assert!(
        o.log
            .contains("REMOTE:powershell -NoProfile -NonInteractive -EncodedCommand "),
        "a Windows strih must be read via -EncodedCommand powershell: {}",
        o.log
    );
}

#[test]
fn a_failed_read_is_empty_and_never_aborts_the_caller_1360() {
    let o = run_case(&format!(
        "{LIB}\nexport STUB_SSH_FAIL=1\na=\"$(strih_log_tail 10.77.9.202 u p 5)\"\n\
         b=\"$(strih_log_line_count 10.77.9.202 u p)\"\nc=\"$(strih_log_since_line 10.0.0.1 u p 3)\"\n\
         strih_log_tail 10.0.0.1 u p 5\necho \"EMPTY:[$a][$b][$c]\""
    ));
    assert_done(&o, "failed read");
    assert!(
        has_line(&o.stdout, "EMPTY:[][][]"),
        "a failed read must yield EMPTY output (fail-open) and return 0: {}",
        o.stdout
    );
}

#[test]
fn since_line_refuses_a_non_numeric_mark_without_any_ssh_1360() {
    let o = run_case(&format!(
        "{LIB}\na=\"$(strih_log_since_line 10.77.9.202 u p '')\"\n\
         b=\"$(strih_log_since_line 10.77.9.202 u p abc)\"\necho \"SINCE:[$a][$b]\""
    ));
    assert_done(&o, "since non-numeric");
    assert!(
        has_line(&o.stdout, "SINCE:[][]"),
        "a garbled mark must never degrade to a whole-log fetch: {}",
        o.stdout
    );
    assert!(
        !o.log.contains("SSHPASS:"),
        "a garbled mark must not even open an ssh: {}",
        o.log
    );
}

#[test]
fn tail_count_is_numeric_clamped_before_the_remote_command_1360() {
    let o = run_case(&format!(
        "{LIB}\necho \"CMD=$(strih_log_remote_cmd linux tail '5; rm -rf /')\"\n\
         echo \"CNT=$(strih_log_remote_cmd linux count)\""
    ));
    assert_done(&o, "tail clamp");
    assert!(
        o.stdout.contains(r#"tail -n 400 "$F""#) && !o.stdout.contains("rm -rf"),
        "a non-numeric tail count must clamp to the default, never splice into the remote shell: {}",
        o.stdout
    );
    assert!(
        o.stdout.contains("ls -t ~/.config/obs-studio/logs/*.txt")
            && o.stdout.contains(r#"wc -l < "$F""#),
        "the Linux reads target the newest ~/.config/obs-studio/logs/*.txt, quoted: {}",
        o.stdout
    );
}

// ---------------------------------------------------------------------------------------------
// (b) every consumer takes the Linux branch on a Linux strih
// ---------------------------------------------------------------------------------------------

#[test]
fn genlock_settle_reads_the_linux_strih_log_1360() {
    let o = run_case(
        ". \"$R/scripts/lib/genlock-settle.sh\"\necho 'SNAP<<'\n\
         _genlock_settle_read_snapshot newlevel pw 10.77.9.202\necho '>>SNAP'",
    );
    assert_done(&o, "genlock-settle");
    assert!(
        o.stdout.contains("received=160"),
        "[4j/8settle] must read the strih-lx OBS log: {}",
        o.stdout
    );
    assert!(
        !o.log.contains("EncodedCommand") && o.log.contains("tail -n 400"),
        "[4j/8settle] must take the Linux branch with its 400-line tail: {}",
        o.log
    );
}

#[test]
fn mv_reverify_received_tap_reads_the_linux_strih_log_1360() {
    let o = run_case(
        ". \"$R/scripts/lib/mv-reverify-escalate.sh\"\n\
         echo \"RECV=$(mv_reverify_probe_received 10.77.9.202 'NDI cam1')\"",
    );
    assert_done(&o, "mv-reverify");
    assert!(
        has_line(&o.stdout, "RECV=160"),
        "the received= tap must read the NEWEST received= off the strih-lx log (not READ_FAIL): {}",
        o.stdout
    );
    assert!(
        !o.log.contains("EncodedCommand"),
        "a Linux strih must never be read the PowerShell way: {}",
        o.log
    );
}

#[test]
fn frozen_cam_gate_tail_reads_the_linux_strih_log_1360() {
    let o = run_case(
        ". \"$R/scripts/lib/frozen-cam-received.sh\"\necho 'RAW<<'\n\
         _frozen_cam_received_read_tail 10.77.9.202\necho '>>RAW'",
    );
    assert_done(&o, "frozen-cam");
    assert!(
        o.stdout.contains("received=160"),
        "[4c/8] must read the strih-lx OBS log: {}",
        o.stdout
    );
    assert!(
        o.log.contains("tail -n 800") && !o.log.contains("EncodedCommand"),
        "[4c/8] keeps its larger 800-line tail on the Linux branch: {}",
        o.log
    );
}

#[test]
fn ndi_cadence_fetch_reads_the_linux_strih_log_1360() {
    let o = run_case(
        ". \"$R/scripts/lib/ndi-cadence-heal.sh\"\necho 'RAW<<'\n\
         _ndi_cadence_fetch 10.77.9.202\necho '>>RAW'",
    );
    assert_done(&o, "ndi-cadence");
    assert!(
        o.stdout.contains("recv-timing #797 'NDI cam1'"),
        "the cadence verify must read the strih-lx recv-timing lines: {}",
        o.stdout
    );
    assert!(
        o.log.contains("SSHPASS:-p newlevel timeout ")
            && o.log.contains("tail -n 800")
            && !o.log.contains("EncodedCommand"),
        "the cadence fetch must take the Linux branch with its receiver creds + 800-line tail: {}",
        o.log
    );
}

#[test]
fn mv_fps_preflight_strih_token_resolves_the_platform_1360() {
    let o = run_case(
        ". \"$R/scripts/lib/mv-fps-preflight.sh\"\necho 'MV<<'\n\
         mv_fps_preflight_probe 10.77.9.202 strih newlevel pw 50\necho '>>MV'\n\
         : > \"$STUB_LOG\"\n\
         export STUB_WIN_OUT='09:23:37.002: multiview-audit: monitor=1 divisor=1 rendered_fps=30.00 target=30 floor=28 cx=1920 cy=1080'\n\
         echo \"WINMV=$(mv_fps_preflight_probe 10.0.0.1 strih newlevel pw 800)\"\n\
         w=\"$(mv_fps_preflight_read_cmd win 800)\"\n\
         if grep -qxF \"REMOTE:$w\" \"$STUB_LOG\"; then echo WIN_IDENTICAL; fi",
    );
    assert_done(&o, "mv-fps preflight");
    let linux = o
        .stdout
        .split("MV<<")
        .nth(1)
        .and_then(|s| s.split(">>MV").next())
        .unwrap_or("");
    assert!(
        linux.contains("rendered_fps=29.97"),
        "[4d1/8] must read the strih-lx multiview-audit line when strih is Linux: {}",
        o.stdout
    );
    assert!(
        o.stdout.contains("WINMV=09:23:37.002: multiview-audit:")
            && has_line(&o.stdout, "WIN_IDENTICAL"),
        "a Windows strih must still get the EXISTING win read_cmd byte-for-byte: {}\nlog:\n{}",
        o.stdout,
        o.log
    );
}

#[test]
fn mv_fps_watchdog_strih_token_resolves_the_platform_1360() {
    let o = run_case(
        "set --\n. \"$R/scripts/mv-fps-alert-watchdog.sh\"\necho \"BOXES=$MV_FPS_BOXES\"\n\
         echo 'P<<'\nprobe_mv_log 10.77.9.202 strih\necho '>>P'",
    );
    assert_done(&o, "mv-fps watchdog");
    assert!(
        o.stdout.contains("BOXES=strih|10.77.9.202|strih"),
        "the watchdog default must name strih with the platform-resolved `strih` os token: {}",
        o.stdout
    );
    assert!(
        o.stdout.contains("MVFPS_LOGID:2026-09-23 09-23-36.txt")
            && o.stdout.contains("rendered_fps=29.97"),
        "the watchdog must read the strih-lx log (identity line + tail) on a Linux strih: {}",
        o.stdout
    );
    assert!(
        !o.log.contains("EncodedCommand"),
        "a Linux strih must never be read the PowerShell way: {}",
        o.log
    );
}

#[test]
fn qr_align_floor_aware_audit_reads_the_linux_strih_log_1360() {
    let o = run_case(
        "install_py_stub\nmkdir -p \"$T/probe\" \"$T/out\"\n\
         printf '#!/usr/bin/env bash\\necho \"{}\"\\n' > \"$T/probe/genlock-jitter-report\"\n\
         chmod +x \"$T/probe/genlock-jitter-report\"\n\
         export STRIH_USER=newlevel PROBE_BIN_DIR=\"$T/probe\" OUTDIR=\"$T/out\" RUN_ID=t1360 \
         QR_ALIGN_SOURCES='NDI cam1' QR_ALIGN_RESET_SETTLE_S=0 QR_ALIGN_AUDIT_WINDOW_S=0\n\
         . \"$R/scripts/lib/qr-align.sh\"\nrc=0\nqr_align_run 10.77.9.202 pw || rc=$?\necho \"QRRC=$rc\"",
    );
    assert_done(&o, "qr-align");
    assert!(
        has_line(&o.stdout, "QRRC=0"),
        "qr_align_run must complete with the stubbed aligner: {}\n{}",
        o.stdout,
        o.stderr
    );
    assert!(
        !o.stderr
            .contains("could not read the post-settle log line count"),
        "the #1161 floor-aware audit must read the strih-lx log line count: {}",
        o.stderr
    );
    assert!(
        o.log.contains("wc -l")
            && o.log.contains("tail -n +6")
            && !o.log.contains("EncodedCommand"),
        "the count (5) + since-line (+6) reads must take the Linux branch: {}",
        o.log
    );
}

// ---------------------------------------------------------------------------------------------
// (c) static wiring
// ---------------------------------------------------------------------------------------------

#[test]
fn consumers_no_longer_carry_their_own_windows_log_reader_1360() {
    for rel in [
        "scripts/lib/qr-align.sh",
        "scripts/lib/genlock-settle.sh",
        "scripts/lib/mv-reverify-escalate.sh",
        "scripts/lib/ndi-cadence-heal.sh",
    ] {
        let s = read(rel);
        assert!(
            !s.contains(r"logs\*.txt") && !s.contains(r"logs\\*.txt"),
            "{rel} must read the strih OBS log through scripts/lib/strih-log-read.sh, not its own \
             Windows-only reader"
        );
        assert!(
            s.contains("strih_log_"),
            "{rel} must call the shared strih_log_* reader"
        );
    }
}

#[test]
fn recording_e2e_mv_fps_preflight_names_strih_with_the_resolved_token_1360() {
    let s = read("scripts/recording-e2e.sh");
    assert_eq!(
        s.matches(r#""strih|$STRIH|strih|$STRIH_USER|$STRIH_PW""#)
            .count(),
        2,
        "both [4d1/8] strih specs must use the platform-resolved `strih` os token"
    );
    assert!(
        !s.contains(r#""strih|$STRIH|win|"#),
        "no [4d1/8] strih spec may hard-code the Windows os token"
    );
}
