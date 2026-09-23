//! issue 1360 part 2 — the REMAINING strih OBS-log readers onto the ONE platform-resolved reader
//! (`scripts/lib/strih-log-read.sh`): the dev1 watchdogs `cadence-alert` (`fetch_box_log`),
//! `frozen-input-alert` (`probe_received`), `ndi-halving` (`fetch_box_log`) and `asio-starve-alert`
//! (`fetch_box_log`), plus the opt-in `[4g/8]` pre-record calibration in `recording-e2e.sh`.
//!
//! Root cause: each still built ONLY the Windows `gc (gci $env:APPDATA\obs-studio\logs\*.txt …)
//! -Tail N` PowerShell read, so against the Linux strih-lx notebook (10.77.9.202, production strih
//! since the M4 cut-over) every read came back EMPTY and each consumer fell back fail-open — a
//! watchdog that silently never pages (UNKNOWN forever).
//!
//! All Tier-0 (no rig, no real ssh), the part-1 recipe (`.claude/rules/strih-log-read.md`): each case
//! is a bash FILE run as `bash <case.sh> <tmpdir>` under `set -euo pipefail`; a PATH-stubbed
//! `sshpass` logs its argv and execs the rest; a PATH-stubbed `ssh` RUNS a Linux remote command for
//! real against a fixture HOME whose newest OBS log has SPACES in its name, and answers a
//! `powershell …` one with a canned Windows reply — so the platform branch actually taken is
//! observable end-to-end, and a Windows box (stream) must keep its PowerShell read.

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

/// Stubs + the fixture HOME. `$1` = the per-case temp dir; the repo root is the cwd (`$R`).
const PRELUDE: &str = r#"set -euo pipefail
T="$1"
shift
R="$PWD"
unset STRIH_PLATFORM STRIH_LX_HOST CADENCE_PROBE_CMD FROZEN_INPUT_PROBE_CMD NDI_HALVING_PROBE_CMD \
  ASIO_STARVE_PROBE_CMD FROZEN_INPUT_ENUMERATE_CMD STUB_SSH_FAIL STUB_WIN_OUT 2>/dev/null || true
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
export PATH="$T/bin:$PATH" STUB_LOG="$T/stub.log" FAKE_HOME="$T/home"
: > "$STUB_LOG"
L="$T/home/.config/obs-studio/logs"
printf 'OLD-LOG-LINE\n' > "$L/2026-09-22 08-00-00.txt"
{
  printf '%s\n' "09:23:36.001: genlock-fifo audit 'NDI cam1': received=100 relocks=0 underruns=0 dropped_due=0 late_holds=0"
  printf '%s\n' "09:23:37.002: asrc: source 'mbc' estimated=+6.0ppm starved_blocks=0 (#803/#806/#960)"
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

/// Source `script` (positional args cleared first — each watchdog parses `$@` at source time) and
/// print the output of `call` between OUT markers. `set +e` after the source restores the
/// watchdogs' own runtime mode (`set -uo pipefail`, NOT `-e`): a grep no-match in a probe's parse
/// pipeline is an ordinary "unread" there, never an abort.
fn watchdog_call(script: &str, call: &str) -> CaseOut {
    run_case(&format!(
        "set --\n. \"$R/{script}\"\nset +e\necho 'OUT<<'\n{call}\necho '>>OUT'"
    ))
}

fn out_block(o: &CaseOut) -> String {
    o.stdout
        .split("OUT<<")
        .nth(1)
        .and_then(|s| s.split(">>OUT").next())
        .unwrap_or("")
        .to_string()
}

/// A Linux strih (strih-lx) read must come back with the newest (spaced-name) log's lines, taken
/// through the shared reader's transport (no PowerShell, no trusted stale known_hosts key).
fn assert_linux_strih_read(o: &CaseOut, ctx: &str) {
    assert_done(o, ctx);
    let out = out_block(o);
    assert!(
        out.contains("received=160") && out.contains("newest-line-5"),
        "{ctx}: a strih-lx read must return the NEWEST log's lines (a spaced filename), got: {out}\nstub log:\n{}",
        o.log
    );
    assert!(
        !out.contains("OLD-LOG-LINE"),
        "{ctx}: must read only the newest log: {out}"
    );
    assert!(
        !o.log.contains("EncodedCommand"),
        "{ctx}: a Linux strih must never be read the PowerShell way: {}",
        o.log
    );
    assert!(
        o.log.contains("-o UserKnownHostsFile=/dev/null"),
        "{ctx}: the strih-lx read must go through the shared reader's transport (no stale \
         known_hosts key disabling password auth): {}",
        o.log
    );
}

/// A Windows box (stream, 10.77.9.204) keeps its existing cmd.exe-proof PowerShell read.
fn assert_windows_read_kept(o: &CaseOut, ctx: &str, want_out: &str) {
    assert_done(o, ctx);
    assert!(
        o.log
            .contains("REMOTE:powershell -NoProfile -NonInteractive -EncodedCommand "),
        "{ctx}: a Windows box must keep its -EncodedCommand PowerShell read: {}",
        o.log
    );
    assert!(
        has_line(&out_block(o), want_out),
        "{ctx}: the Windows reply must come back through the unchanged parse ({want_out}): {}",
        o.stdout
    );
}

// ---------------------------------------------------------------------------------------------
// (a) each watchdog's strih read on strih-lx
// ---------------------------------------------------------------------------------------------

#[test]
fn cadence_watchdog_reads_the_linux_strih_log_1360() {
    let o = watchdog_call(
        "scripts/cadence-alert-watchdog.sh",
        "fetch_box_log 10.77.9.202",
    );
    assert_linux_strih_read(&o, "cadence fetch_box_log");
}

#[test]
fn frozen_input_probe_received_reads_the_linux_strih_log_1360() {
    let o = watchdog_call(
        "scripts/frozen-input-alert-watchdog.sh",
        "probe_received 10.77.9.202 'NDI cam1'",
    );
    assert_done(&o, "frozen-input probe_received");
    assert!(
        has_line(&out_block(&o), "160"),
        "probe_received on strih-lx must return the newest received= of the source (160): {}\nstub log:\n{}",
        o.stdout,
        o.log
    );
    assert!(
        !o.log.contains("EncodedCommand") && o.log.contains("-o UserKnownHostsFile=/dev/null"),
        "probe_received must read a Linux strih through the shared reader, never PowerShell: {}",
        o.log
    );
}

#[test]
fn ndi_halving_watchdog_reads_the_linux_strih_log_1360() {
    let o = watchdog_call(
        "scripts/ndi-halving-watchdog.sh",
        "fetch_box_log 10.77.9.202",
    );
    assert_linux_strih_read(&o, "ndi-halving fetch_box_log");
    assert!(
        out_block(&o).contains("recv-timing #797 'NDI cam1'"),
        "the halving tap line must be in the strih-lx read: {}",
        o.stdout
    );
}

#[test]
fn asio_starve_watchdog_reads_the_linux_strih_log_1360() {
    let o = watchdog_call(
        "scripts/asio-starve-alert-watchdog.sh",
        "fetch_box_log 10.77.9.202",
    );
    assert_linux_strih_read(&o, "asio-starve fetch_box_log");
    assert!(
        out_block(&o).contains("asrc: source 'mbc'"),
        "the asrc tap line must be in the strih-lx read: {}",
        o.stdout
    );
}

#[test]
fn a_failed_strih_lx_read_stays_empty_and_never_aborts_1360() {
    for (script, call) in [
        (
            "scripts/cadence-alert-watchdog.sh",
            "fetch_box_log 10.77.9.202",
        ),
        (
            "scripts/frozen-input-alert-watchdog.sh",
            "probe_received 10.77.9.202 'NDI cam1'",
        ),
        (
            "scripts/ndi-halving-watchdog.sh",
            "fetch_box_log 10.77.9.202",
        ),
        (
            "scripts/asio-starve-alert-watchdog.sh",
            "fetch_box_log 10.77.9.202",
        ),
    ] {
        let o = run_case(&format!(
            "export STUB_SSH_FAIL=1\nset --\n. \"$R/{script}\"\nset +e\nv=\"$({call})\"\necho \"EMPTY:[$v]\""
        ));
        assert_done(&o, script);
        assert!(
            has_line(&o.stdout, "EMPTY:[]"),
            "{script}: a failed strih-lx read must be EMPTY (the watchdog's UNKNOWN path): {}",
            o.stdout
        );
    }
}

// ---------------------------------------------------------------------------------------------
// (b) the Windows boxes (stream) keep their own read; the probe seams still win
// ---------------------------------------------------------------------------------------------

#[test]
fn windows_boxes_keep_their_powershell_read_1360() {
    for (script, call) in [
        (
            "scripts/cadence-alert-watchdog.sh",
            "fetch_box_log 10.77.9.204",
        ),
        (
            "scripts/ndi-halving-watchdog.sh",
            "fetch_box_log 10.77.9.204",
        ),
        (
            "scripts/asio-starve-alert-watchdog.sh",
            "fetch_box_log 10.77.9.204",
        ),
    ] {
        let o = run_case(&format!(
            "export STUB_WIN_OUT=WIN-TAIL-TEXT\nset --\n. \"$R/{script}\"\nset +e\necho 'OUT<<'\n{call}\necho '>>OUT'"
        ));
        assert_windows_read_kept(&o, script, "WIN-TAIL-TEXT");
    }
    let o = run_case(
        "export STUB_WIN_OUT=\"09:00:00.000: genlock-fifo audit 'NDI cam1': received=77 x WIN-TAIL-TEXT\"\n\
         set --\n. \"$R/scripts/frozen-input-alert-watchdog.sh\"\nset +e\necho 'OUT<<'\n\
         probe_received 10.77.9.204 'NDI cam1'\necho '>>OUT'",
    );
    assert_windows_read_kept(&o, "frozen-input probe_received (windows)", "77");
}

#[test]
fn probe_cmd_seams_still_override_the_strih_lx_read_1360() {
    for (script, var, call) in [
        (
            "scripts/cadence-alert-watchdog.sh",
            "CADENCE_PROBE_CMD",
            "fetch_box_log 10.77.9.202",
        ),
        (
            "scripts/ndi-halving-watchdog.sh",
            "NDI_HALVING_PROBE_CMD",
            "fetch_box_log 10.77.9.202",
        ),
        (
            "scripts/asio-starve-alert-watchdog.sh",
            "ASIO_STARVE_PROBE_CMD",
            "fetch_box_log 10.77.9.202",
        ),
    ] {
        let o = run_case(&format!(
            "printf '#!/usr/bin/env bash\\necho SEAM-READ\\n' > \"$T/bin/seam\"\nchmod +x \"$T/bin/seam\"\n\
             export {var}=seam\nset --\n. \"$R/{script}\"\nset +e\necho 'OUT<<'\n{call}\necho '>>OUT'"
        ));
        assert_done(&o, script);
        assert!(
            has_line(&out_block(&o), "SEAM-READ") && !o.log.contains("SSHPASS:"),
            "{script}: the {var} dry-run/fixture seam must still replace the whole read: {}\n{}",
            o.stdout,
            o.log
        );
    }
}

// ---------------------------------------------------------------------------------------------
// (c) static wiring
// ---------------------------------------------------------------------------------------------

#[test]
fn watchdogs_source_the_shared_strih_reader_1360() {
    for rel in [
        "scripts/cadence-alert-watchdog.sh",
        "scripts/frozen-input-alert-watchdog.sh",
        "scripts/ndi-halving-watchdog.sh",
        "scripts/asio-starve-alert-watchdog.sh",
    ] {
        let s = read(rel);
        assert!(
            s.contains(r#". "$HERE/lib/strih-log-read.sh""#),
            "{rel} must source the ONE shared strih OBS-log reader"
        );
        assert!(
            s.contains("strih_log_tail "),
            "{rel} must read a Linux strih through strih_log_tail"
        );
    }
}

/// The `[4g/8]` pre-record calibration block (opt-in `PRERECORD_PHASE_CALIBRATE=1`).
fn calib_block() -> String {
    let s = read("scripts/recording-e2e.sh");
    let start = s
        .find(r#"if [ "$PRERECORD_PHASE_CALIBRATE" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ]; then"#)
        .expect("[4g/8] calibration guard");
    let rest = &s[start..];
    let end = rest
        .find("pre-record phase auto-pin — SKIPPED")
        .expect("[4g/8] SKIPPED branch");
    rest[..end].to_string()
}

#[test]
fn prerecord_calibration_reads_strih_through_the_shared_reader_1360() {
    let b = calib_block();
    assert!(
        !b.contains(r"logs\*.txt"),
        "[4g/8] must not build its own Windows-only strih OBS-log read"
    );
    assert!(
        b.contains(r#". "$HERE/lib/strih-log-read.sh""#),
        "[4g/8] must source the shared reader itself, never rely on a transitive source"
    );
    assert!(
        b.contains(r#"strih_log_line_count "$STRIH" "$STRIH_USER" "$STRIH_PW""#),
        "[4g/8] must mark the log length with strih_log_line_count (Correction-2 time scoping)"
    );
    assert!(
        b.contains(
            r#"strih_log_since_line "$STRIH" "$STRIH_USER" "$STRIH_PW" "$CALIB_LOG_START_LINES""#
        ),
        "[4g/8] must fetch ONLY the post-mark lines with strih_log_since_line"
    );
}

#[test]
fn prerecord_calibration_mark_and_fetch_scope_the_linux_log_1360() {
    // Run the [4g/8] mark + fetch statements EXACTLY as extracted from recording-e2e.sh (the only
    // two strih-touching statements besides the python calls) against the fixture strih-lx.
    let b = calib_block();
    let mark = b
        .lines()
        .skip_while(|l| !l.contains("CALIB_LOG_START_LINES=\"$(strih_log_line_count"))
        .take_while(|l| !l.contains("case \"$CALIB_LOG_START_LINES\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!mark.is_empty(), "[4g/8] mark statement not found");
    // The fetch is the `if <fetch> ... && [ -s "$CALIB_LOG" ]; then` condition: take it from its
    // `if strih_log_since_line` line through the `; then` line, and run it as a plain command.
    let fetch_lines: Vec<&str> = b
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("if strih_log_since_line"))
        .collect();
    let end = fetch_lines
        .iter()
        .position(|l| l.trim_end().ends_with("; then"))
        .expect("[4g/8] fetch condition end");
    let fetch = fetch_lines[..=end]
        .join("\n")
        .trim()
        .trim_start_matches("if ")
        .trim_end_matches("; then")
        .to_string();
    let o = run_case(&format!(
        ". \"$R/scripts/lib/strih-log-read.sh\"\nSTRIH=10.77.9.202 STRIH_USER=newlevel STRIH_PW=pw\n\
         {mark}\necho \"MARK=$CALIB_LOG_START_LINES\"\nCALIB_LOG_START_LINES=3\nCALIB_LOG=\"$T/calib.log\"\n\
         if {fetch}; then echo FETCH_OK; fi\necho 'OUT<<'\ncat \"$CALIB_LOG\"\necho '>>OUT'"
    ));
    assert_done(&o, "[4g/8] mark");
    assert!(
        has_line(&o.stdout, "MARK=5"),
        "[4g/8] must mark the strih-lx log length: {}\n{}",
        o.stdout,
        o.log
    );
    assert!(
        has_line(&o.stdout, "FETCH_OK"),
        "the extracted [4g/8] fetch statement must succeed on strih-lx: {}\n{}",
        o.stdout,
        o.log
    );
    let out = out_block(&o);
    assert!(
        out.contains("received=160") && !out.contains("received=100"),
        "the post-mark fetch must write only lines after the mark into CALIB_LOG: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// (d) part 3 — the genlock-fifo audit BEFORE/AFTER snapshot (`scripts/lib/genlock-audit-snapshot.sh`,
// the issue-1354 scope-3 producer) reads strih through the SAME shared reader: its own inline Linux
// ssh + remote grep and its Windows SKIP are gone; the audit-line filter + the 400-line cap stay
// local, the `GENLOCK_AUDIT_SNAPSHOT_READER_CMD` seam and the persisted file format are unchanged.
// ---------------------------------------------------------------------------------------------

/// Source the shared reader (so `strih_log_remote_cmd` can compute the expected remote command
/// even before the snapshot lib sources it itself) and the snapshot lib, clear the knobs, run `body`.
fn snapshot_case(body: &str) -> CaseOut {
    run_case(&format!(
        ". \"$R/scripts/lib/strih-log-read.sh\"\n. \"$R/scripts/lib/genlock-audit-snapshot.sh\"\n\
         unset GENLOCK_AUDIT_SNAPSHOT_READER_CMD GENLOCK_AUDIT_SNAPSHOT_TAIL \
         GENLOCK_AUDIT_SNAPSHOT_READ_LINES GENLOCK_AUDIT_SNAPSHOT_SSH_TIMEOUT STRIH_USER STRIH_PW\n\
         {body}"
    ))
}

/// The expected remote command the case printed as `WANT=<cmd>`.
fn want_line(o: &CaseOut) -> String {
    o.stdout
        .lines()
        .find_map(|l| l.strip_prefix("WANT="))
        .expect("WANT line")
        .to_string()
}

fn non_empty_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.is_empty()).collect()
}

const AUDIT_100: &str = "09:23:36.001: genlock-fifo audit 'NDI cam1': received=100 relocks=0 underruns=0 dropped_due=0 late_holds=0";
const AUDIT_160: &str = "09:23:39.004: genlock-fifo audit 'NDI cam1': received=160 relocks=0 underruns=0 dropped_due=0 late_holds=0";

#[test]
fn genlock_audit_snapshot_reads_the_linux_strih_log_through_the_shared_reader_1360() {
    let o = snapshot_case(
        "export STRIH=10.77.9.202 STRIH_USER=opuser STRIH_PW=secretpw \
         GENLOCK_AUDIT_SNAPSHOT_SSH_TIMEOUT=5\n\
         genlock_audit_snapshot_capture before \"$T/before.txt\"\n\
         echo \"WANT=$(strih_log_remote_cmd linux tail 3000)\"\n\
         echo 'OUT<<'\ncat \"$T/before.txt\"\necho '>>OUT'",
    );
    assert_done(&o, "genlock-audit snapshot (strih-lx)");
    let want = want_line(&o);
    assert!(
        !want.is_empty() && has_line(&o.log, &format!("REMOTE:{want}")),
        "the snapshot must read strih-lx with the shared reader's raw tail (default 3000 lines), \
         never its own remote grep; want REMOTE:{want}\nstub log:\n{}",
        o.log
    );
    assert!(
        o.log.contains("-p secretpw timeout 5 ssh ")
            && o.log.contains("-o UserKnownHostsFile=/dev/null")
            && o.log.contains("opuser@10.77.9.202"),
        "STRIH_USER / STRIH_PW / GENLOCK_AUDIT_SNAPSHOT_SSH_TIMEOUT must reach the shared \
         reader's transport: {}",
        o.log
    );
    let out = out_block(&o);
    assert_eq!(
        non_empty_lines(&out),
        [AUDIT_100, AUDIT_160],
        "the persisted file must hold ONLY the newest log's genlock-fifo audit lines, verbatim \
         (the asrc / recv-timing / other lines filtered locally), got: {out}"
    );
}

#[test]
fn genlock_audit_snapshot_reads_a_windows_strih_instead_of_skipping_1360() {
    let o = snapshot_case(
        "export STRIH=10.77.9.204 STRIH_PLATFORM=windows\n\
         export STUB_WIN_OUT=$'09:00:00.000: genlock-fifo audit \\'NDI cam1\\': received=77 holds=1\\r\\n09:00:01.000: other-line\\r'\n\
         genlock_audit_snapshot_capture after \"$T/after.txt\"\n\
         echo 'OUT<<'\ncat \"$T/after.txt\" 2>/dev/null || echo NO-FILE\necho '>>OUT'",
    );
    assert_done(&o, "genlock-audit snapshot (windows strih)");
    assert!(
        o.log
            .contains("REMOTE:powershell -NoProfile -NonInteractive -EncodedCommand "),
        "a Windows strih must be read through the shared reader's -EncodedCommand tail, not \
         skipped: {}\n{}",
        o.log,
        o.stderr
    );
    let out = out_block(&o);
    assert_eq!(
        non_empty_lines(&out),
        ["09:00:00.000: genlock-fifo audit 'NDI cam1': received=77 holds=1"],
        "the Windows read must persist only the audit line: {out:?}"
    );
    // `str::lines()` already drops a `\r` before `\n`, so assert on the raw bytes too.
    assert!(
        !out.contains('\r'),
        "the persisted Windows audit line must be CR-stripped: {out:?}"
    );
}

#[test]
fn genlock_audit_snapshot_knobs_stay_local_and_injection_safe_1360() {
    // The audit-line cap is applied LOCALLY to the filtered lines (TAIL=1 -> only the newest).
    let o = snapshot_case(
        "export STRIH=10.77.9.202 GENLOCK_AUDIT_SNAPSHOT_TAIL=1\n\
         genlock_audit_snapshot_capture before \"$T/b.txt\"\n\
         echo 'OUT<<'\ncat \"$T/b.txt\"\necho '>>OUT'",
    );
    assert_done(&o, "TAIL=1");
    let out = out_block(&o);
    assert_eq!(
        non_empty_lines(&out),
        [AUDIT_160],
        "TAIL=1 keeps only the newest audit line: {out}"
    );
    // Non-numeric overrides fall back to the defaults and never reach the remote command.
    let o = snapshot_case(
        "export STRIH=10.77.9.202 GENLOCK_AUDIT_SNAPSHOT_TAIL='x; rm -rf /' \
         GENLOCK_AUDIT_SNAPSHOT_READ_LINES='y; rm -rf /'\n\
         genlock_audit_snapshot_capture before \"$T/b.txt\"\n\
         echo \"WANT=$(strih_log_remote_cmd linux tail 3000)\"\n\
         echo 'OUT<<'\ncat \"$T/b.txt\"\necho '>>OUT'",
    );
    assert_done(&o, "non-numeric knobs");
    let want = want_line(&o);
    assert!(
        has_line(&o.log, &format!("REMOTE:{want}")) && !o.log.contains("rm -rf"),
        "a non-numeric READ_LINES falls back to 3000 with no injection: {}",
        o.log
    );
    let out = out_block(&o);
    assert_eq!(
        non_empty_lines(&out),
        [AUDIT_100, AUDIT_160],
        "a non-numeric TAIL falls back to 400 (both audit lines kept): {out}"
    );
}

#[test]
fn genlock_audit_snapshot_failed_strih_read_writes_no_file_1360() {
    let o = snapshot_case(
        "export STRIH=10.77.9.202 STUB_SSH_FAIL=1\n\
         genlock_audit_snapshot_capture before \"$T/b.txt\"\n\
         if [ -e \"$T/b.txt\" ]; then echo FILE-EXISTS; else echo NO-FILE; fi",
    );
    assert_done(&o, "failed read");
    assert!(
        has_line(&o.stdout, "NO-FILE"),
        "a failed strih read must leave no file (the report omits the section): {}",
        o.stdout
    );
}

#[test]
fn genlock_audit_snapshot_sources_the_shared_reader_and_owns_no_read_1360() {
    let s = read("scripts/lib/genlock-audit-snapshot.sh");
    assert!(
        s.contains(r#"/strih-log-read.sh""#) && s.contains("strih_log_tail "),
        "the snapshot lib must source and read through the ONE shared strih OBS-log reader"
    );
    for banned in ["obs-studio/logs", "sshpass", "APPDATA"] {
        assert!(
            !s.contains(banned),
            "the snapshot lib must not build its own strih-log read (found `{banned}`)"
        );
    }
}
