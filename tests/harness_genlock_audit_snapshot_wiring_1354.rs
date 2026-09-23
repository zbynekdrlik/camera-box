//! #1354 scope 3 remainder — wire the genlock-fifo audit BEFORE/AFTER snapshot into the full-path
//! E2E harness so the per-run report NAMES the conveyor-ladder victim input.
//!
//! The pure parser (`scripts/genlock_audit_snapshot.py`) and the report consumer
//! (`e2e_discord_report.py _section_genlock_conveyor`, forwarded as `e2e_discord_report_send`'s 7th
//! arg by `scripts/lib/e2e-discord-report.sh`) are already merged (release PR #1348). This ticket's
//! remainder is the PRODUCER step: `scripts/recording-e2e.sh` must (a) capture strih's per-input
//! `genlock-fifo audit '<name>':` tail BEFORE the record step and AFTER the stop step, (b) compute
//! the window deltas, and (c) pass the resulting JSON as the report send's 7th arg. All of it is
//! report-only and fail-open — it never touches `$GATE`.
//!
//! Two test families, same discipline as tests/harness_genlock_settle_1221.rs +
//! tests/harness_recording_e2e_mv_skew_761.rs:
//!   * FUNCTIONAL — source the REAL sourced helper `scripts/lib/genlock-audit-snapshot.sh` and
//!     exercise its two runner functions under the caller's `set -euo pipefail` (fail-open proof:
//!     an empty read writes no file and exits 0; a two-tail fixture yields the per-input deltas).
//!   * STATIC ANCHORS — the helper is sourced once, `capture before` precedes the [5/8] record
//!     step, `capture after` follows the [7/8] stop, `compute` runs after the merge and before the
//!     send, and the send passes the genlock-audit JSON as its 7th argument.
//!
//! RED before the lib + wiring exist (the file is absent, every test fails); GREEN after. `cargo`
//! does NOT run locally here (Tier-0, build-ok DISABLED #557) — the observable local red->green is
//! a bash replica sourcing the lib; CI runs these assertions.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/genlock-audit-snapshot.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn read(p: &str) -> String {
    let path = manifest_dir().join(p);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Source the REAL lib under `set -euo pipefail` (the caller's strict mode — recording-e2e.sh) and
/// run `body`. Returns (exit, stdout, stderr). A fail-open helper must survive `set -e`.
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}", body = body);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .env(
            "GENLOCK_AUDIT_SNAPSHOT_HERE",
            manifest_dir().join("scripts"),
        )
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// FUNCTIONAL — the lib's two runner functions
// ---------------------------------------------------------------------------------------------

#[test]
fn lib_defines_the_runner_functions() {
    for f in [
        "genlock_audit_snapshot_capture",
        "genlock_audit_snapshot_compute",
    ] {
        let (rc, out, err) = run_sourced(&format!(
            "type {f} >/dev/null 2>&1 && echo DEFINED || echo MISSING"
        ));
        assert_eq!(rc, 0, "harness rc={rc} stderr={err}");
        assert_eq!(out.trim(), "DEFINED", "{f} must be defined by the lib");
    }
}

/// An empty read (the reader seam returns nothing) must write NO file and STILL exit 0 — the
/// report composer's `[ -s ]` guard then omits the section. Fail-open under `set -euo pipefail`.
#[test]
fn capture_empty_read_writes_no_file_and_exits_zero() {
    let body = r#"
d="$(mktemp -d)"
out="$d/genlock-audit-before.txt"
export GENLOCK_AUDIT_SNAPSHOT_READER_CMD='true'   # reader produces no output
genlock_audit_snapshot_capture before "$out"
rc=$?
[ "$rc" = 0 ] || { echo "NONZERO_RC=$rc"; exit 7; }
[ -e "$out" ] && { echo "FILE_SHOULD_NOT_EXIST"; exit 8; }
echo OK
"#;
    let (rc, out, err) = run_sourced(body);
    assert_eq!(
        rc, 0,
        "capture must fail-open (rc={rc}) stderr={err}\n{out}"
    );
    assert!(out.contains("OK"), "stdout={out} stderr={err}");
}

/// A non-empty read is persisted verbatim to the outfile so the compute step can parse it.
#[test]
fn capture_persists_the_read_tail_to_the_outfile() {
    // The reader seam echoes two audit lines (one input); capture must write them to the outfile.
    let body = r#"
d="$(mktemp -d)"
out="$d/genlock-audit-before.txt"
reader() { printf "%s\n" "00:00:01.000: genlock-fifo audit 'NDI cam1': received=10 holds=1 relocks=0 converge_sheds=0 depth=15 dropped_due=5"; }
export -f reader
export GENLOCK_AUDIT_SNAPSHOT_READER_CMD='reader'
genlock_audit_snapshot_capture before "$out"
[ -s "$out" ] || { echo "OUTFILE_EMPTY_OR_ABSENT"; exit 9; }
grep -q "genlock-fifo audit 'NDI cam1'" "$out" || { echo "MISSING_AUDIT_LINE"; exit 10; }
echo OK
"#;
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "rc={rc} stderr={err}\n{out}");
    assert!(out.contains("OK"), "stdout={out} stderr={err}");
}

/// A BEFORE + AFTER two-tail fixture through `compute` yields the per-input window-delta JSON, and
/// the input with the largest positive `holds` delta is named the victim (mirrors the merged
/// parser's `compute_window_deltas`).
#[test]
fn compute_two_tail_fixture_yields_per_input_delta_json_with_victim() {
    let body = r#"
d="$(mktemp -d)"
before="$d/before.txt"; after="$d/after.txt"; outj="$d/deltas.json"
{
  printf "%s\n" "00:00:01.000: genlock-fifo audit 'NDI cam1': received=1000 holds=5 relocks=1 converge_sheds=0 depth=15 dropped_due=100"
  printf "%s\n" "00:00:01.000: genlock-fifo audit 'NDI cam3': received=1000 holds=2 relocks=0 converge_sheds=0 depth=15 dropped_due=100"
} > "$before"
{
  printf "%s\n" "00:05:01.000: genlock-fifo audit 'NDI cam1': received=9000 holds=40 relocks=6 converge_sheds=3 depth=15 dropped_due=900"
  printf "%s\n" "00:05:01.000: genlock-fifo audit 'NDI cam3': received=9000 holds=3 relocks=0 converge_sheds=0 depth=15 dropped_due=900"
} > "$after"
genlock_audit_snapshot_compute "$before" "$after" "$outj"
[ -s "$outj" ] || { echo "NO_JSON"; exit 11; }
grep -q '"victim": "NDI cam1"' "$outj" || { echo "WRONG_VICTIM"; cat "$outj"; exit 12; }
grep -q '"holds": 35' "$outj" || { echo "WRONG_HOLDS_DELTA"; cat "$outj"; exit 13; }
echo OK
"#;
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "rc={rc} stderr={err}\n{out}");
    assert!(out.contains("OK"), "stdout={out} stderr={err}");
}

/// A missing/empty tail (an unreadable strih, an early abort) must write NO JSON and exit 0 —
/// never a python traceback that could abort the run under `set -e`.
#[test]
fn compute_missing_tail_writes_no_json_and_exits_zero() {
    let body = r#"
d="$(mktemp -d)"
outj="$d/deltas.json"
genlock_audit_snapshot_compute "$d/absent-before.txt" "$d/absent-after.txt" "$outj"
rc=$?
[ "$rc" = 0 ] || { echo "NONZERO_RC=$rc"; exit 14; }
[ -e "$outj" ] && { echo "JSON_SHOULD_NOT_EXIST"; exit 15; }
echo OK
"#;
    let (rc, out, err) = run_sourced(body);
    assert_eq!(
        rc, 0,
        "compute must fail-open (rc={rc}) stderr={err}\n{out}"
    );
    assert!(out.contains("OK"), "stdout={out} stderr={err}");
}

/// Issue 1360 part 3: the snapshot no longer owns a strih-log reader of its own. The former
/// Linux-only ssh read (`genlock_audit_snapshot_linux_read`: its own remote grep, and a logged SKIP
/// on a Windows strih) is replaced by the ONE shared reader `scripts/lib/strih-log-read.sh`
/// (`strih_log_tail`) plus a LOCAL audit-line filter. The live-read contract (remote command
/// identity, the tail/read-window sanitization, both platforms, the fail-open empty read) is
/// pinned end-to-end in tests/harness_strih_log_read_consumers_1360.rs section (d); here only
/// that the per-helper reader is gone.
#[test]
fn lib_owns_no_private_strih_reader_1360() {
    let (rc, out, err) = run_sourced(
        "type genlock_audit_snapshot_linux_read >/dev/null 2>&1 && echo DEFINED || echo GONE",
    );
    assert_eq!(rc, 0, "harness rc={rc} stderr={err}");
    assert_eq!(
        out.trim(),
        "GONE",
        "the snapshot lib must read strih through the shared reader, never its own ssh read"
    );
}

// ---------------------------------------------------------------------------------------------
// STATIC ANCHORS — the recording-e2e.sh wiring (#675 anchor-safe: only NEW call lines added)
// ---------------------------------------------------------------------------------------------

#[test]
fn helper_is_sourced_exactly_once() {
    let s = read("scripts/recording-e2e.sh");
    let n = s
        .matches(". \"$HERE/lib/genlock-audit-snapshot.sh\"")
        .count();
    assert_eq!(
        n, 1,
        "#1354: scripts/lib/genlock-audit-snapshot.sh must be sourced exactly once (found {n})"
    );
}

#[test]
fn capture_before_precedes_the_record_step() {
    let s = read("scripts/recording-e2e.sh");
    let cap = s
        .find("genlock_audit_snapshot_capture before")
        .expect("#1354: the BEFORE-window capture call must exist");
    let record = s
        .find("echo \"[5/8] StartRecord")
        .expect("the [5/8] record step banner must exist");
    assert!(
        cap < record,
        "#1354: the BEFORE-window genlock-audit capture must run BEFORE the [5/8] record step \
         (so its delta baseline predates the recording window)"
    );
}

#[test]
fn capture_after_follows_the_stop_step() {
    let s = read("scripts/recording-e2e.sh");
    let stop = s
        .find("record --host \"$STRIH\"  --action stop")
        .expect("the [7/8] strih StopRecord call must exist");
    let cap = s
        .find("genlock_audit_snapshot_capture after")
        .expect("#1354: the AFTER-window capture call must exist");
    assert!(
        cap > stop,
        "#1354: the AFTER-window genlock-audit capture must run AFTER the [7/8] stop step (so its \
         delta closes at the end of the recording window)"
    );
}

#[test]
fn compute_runs_after_the_merge_and_before_the_report_send() {
    let s = read("scripts/recording-e2e.sh");
    let merge = s
        .find("\"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\"")
        .expect("the merge recording-verdict execution must exist");
    let compute = s
        .find("genlock_audit_snapshot_compute")
        .expect("#1354: the compute call must exist");
    let send = s
        .find("e2e_discord_report_send \"$REPORT_JSON\"")
        .expect("the Discord report send call must exist");
    assert!(
        merge < compute && compute < send,
        "#1354: compute must run AFTER the merge (the window is over) and BEFORE the report send \
         (so the deltas land in the SAME report)"
    );
}

#[test]
fn report_send_receives_the_genlock_audit_json_as_seventh_arg() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(
            "e2e_discord_report_send \"$REPORT_JSON\" \"$RUN_ID\" \"$GATE\" \"$DURATION\" \
             \"$PINS_JSON\" \"$MV_SKEW_JSON\" \"$GENLOCK_AUDIT_JSON\""
        ),
        "#1354: e2e_discord_report_send must be called with the genlock-audit JSON as its 7th arg"
    );
}

#[test]
fn compute_and_capture_wiring_is_fail_open() {
    let s = read("scripts/recording-e2e.sh");
    // The compute call carries a trailing `|| true` (belt-and-suspenders on top of the helper's own
    // return 0), and no `exit 1`/`GATE=1` sits in its immediate block — a snapshot failure is
    // report-only and must never change the run's exit code.
    let idx = s
        .find("genlock_audit_snapshot_compute")
        .expect("#1354: the compute call must exist");
    let block = &s[idx..(idx + 260).min(s.len())];
    assert!(
        block.contains("|| true"),
        "#1354: the compute call must be fail-open (|| true): {block}"
    );
    assert!(
        !block.contains("exit 1") && !block.contains("GATE=1"),
        "#1354: a genlock-audit snapshot failure must NEVER touch $GATE / the exit code: {block}"
    );
}
