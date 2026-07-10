//! #652 — E2E recordings pile up forever on strih/stream disks.
//!
//! ## The problem (live incident, 2026-07-10)
//!
//! strih's disk was down to 17 GB free: ~500 GB of forgotten camera-box E2E test recordings had
//! accumulated since 2026-06-17 (incl. a single 266 GB stray), + 139 GB on stream. The harness
//! records every run's strih+stream program but NEVER deletes it — every run's recording (plus
//! any #649-style stray leftover) stays forever.
//!
//! ## The fix these tests lock (static read of the shell script — no rig, no ssh)
//!
//! 1. A disk-budget preflight WARN (never fail — informational): recording-e2e.sh curls each
//!    box's new `/record-dir-stats.json` (bundle-state-server.py + bundle_state_gather.py's pure
//!    `record_dir_stats()`) and logs a loud WARNING when the accumulated recordings exceed
//!    RECORDINGS_BUDGET_GB.
//! 2. After a successful decode+merge (the verdict JSON secured at $REPORT_JSON), the harness
//!    prints the EXACT, run-scoped deletion plan for ONLY this run's own strih+stream recordings
//!    — via the box-local `$STRIH_HOST_PATH` / `$STREAM_HOST_PATH` StopRecord returned earlier
//!    (mirrors the #649 exact-path-tracking discipline) — for an agent/operator to execute via
//!    the win-* MCP (scp/ssh to Windows is denied, the same constraint every other Windows-side
//!    step in this script already works under). NEVER a directory sweep. `KEEP_RECORDINGS=1`
//!    opts out (debugging). Printed in BOTH the default per-box (VERDICT_ON_STREAM=1) plan AND
//!    the legacy fused (VERDICT_ON_STREAM=0) path, which actually runs the verdict synchronously
//!    and knows GATE before its own final `exit "$GATE"`.

use std::fs;

fn read_recording_e2e() -> String {
    let path = format!("{}/scripts/recording-e2e.sh", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The disk-budget preflight must exist: a configurable budget, a curl to the new
/// /record-dir-stats.json endpoint on both strih and stream, and a #652-tagged WARNING.
#[test]
fn recording_e2e_sh_has_disk_budget_preflight_warn() {
    let s = read_recording_e2e();
    assert!(
        s.contains("RECORDINGS_BUDGET_GB"),
        "#652: recording-e2e.sh must define an overridable RECORDINGS_BUDGET_GB"
    );
    assert!(
        s.contains("record-dir-stats.json"),
        "#652: recording-e2e.sh must curl the new /record-dir-stats.json endpoint"
    );
    assert!(
        s.contains("WARNING #652"),
        "#652: an over-budget recordings dir must WARN loudly, tagged #652"
    );
    // Called for BOTH boxes — never just one.
    let strih_call = s.find("check_recordings_budget strih");
    let stream_call = s.find("check_recordings_budget stream");
    assert!(
        strih_call.is_some() && stream_call.is_some(),
        "#652: the disk-budget check must run against BOTH strih and stream"
    );
}

/// The disk-budget WARN must never abort the run — it is informational only (mirrors every
/// other best-effort preflight facet in this script, e.g. fetch_box_state's `|| true`).
#[test]
fn recording_e2e_sh_disk_budget_check_is_never_fatal() {
    let s = read_recording_e2e();
    let fn_start = s
        .find("check_recordings_budget()")
        .expect("#652: recording-e2e.sh must define check_recordings_budget()");
    let fn_end = s[fn_start..]
        .find("\n}\n")
        .map(|i| fn_start + i)
        .expect("check_recordings_budget() must have a closing brace");
    let body = &s[fn_start..fn_end];
    assert!(
        body.contains("return 0"),
        "#652: an unreachable bundle-state-server must degrade gracefully (return 0), never \
         abort the whole run: {body}"
    );
}

/// The per-box (default VERDICT_ON_STREAM=1) plan must print the run-scoped cleanup
/// instructions AFTER the merge command it already prints, and before that branch's `exit 0`.
#[test]
fn recording_e2e_sh_per_box_path_prints_cleanup_after_the_merge_command() {
    let s = read_recording_e2e();
    let merge_print_pos = s
        .find(r#"printf '      %q ' "$VERDICT_BIN" "${MERGE_ARGS[@]}"; echo"#)
        .expect("recording-e2e.sh must still print the merge command");
    let cleanup_pos = s
        .find("#652")
        .filter(|&p| p > merge_print_pos)
        .or_else(|| {
            // fall back to a scan of every #652 occurrence, pick the first one after the merge print
            let mut idx = 0;
            loop {
                match s[idx..].find("#652") {
                    Some(rel) => {
                        let abs = idx + rel;
                        if abs > merge_print_pos {
                            break Some(abs);
                        }
                        idx = abs + 1;
                    }
                    None => break None,
                }
            }
        })
        .expect("#652: a #652-tagged cleanup instruction must appear after the merge print");
    assert!(
        cleanup_pos > merge_print_pos,
        "#652 cleanup instructions must come AFTER the merge command print"
    );

    // The per-box branch's own `exit 0` (the "harness only EMITS the plan" exit) must come
    // AFTER the cleanup print, not before it.
    let per_box_exit_marker = "PASS/FAIL is the merge recording-verdict EXIT CODE on dev1";
    let exit_marker_pos = s
        .find(per_box_exit_marker)
        .expect("the per-box branch's own NOTE about exit 0 must still exist");
    assert!(
        cleanup_pos > exit_marker_pos,
        "#652: the cleanup plan must be printed in the per-box (VERDICT_ON_STREAM=1) branch, \
         after its own exit-code NOTE"
    );
}

/// The legacy (VERDICT_ON_STREAM=0) fused path actually runs the verdict synchronously and
/// knows GATE — its cleanup print must land before the script's FINAL `exit "$GATE"`.
#[test]
fn recording_e2e_sh_legacy_path_prints_cleanup_before_final_exit_gate() {
    let s = read_recording_e2e();
    let final_exit_pos = s
        .rfind("exit \"$GATE\"")
        .expect("recording-e2e.sh must still exit \"$GATE\" at the end of the legacy path");
    // The nearest #652 mention before the final exit is the legacy cleanup print.
    let mut last_652_before_exit = None;
    let mut idx = 0;
    while let Some(rel) = s[idx..final_exit_pos].find("#652") {
        last_652_before_exit = Some(idx + rel);
        idx = idx + rel + 1;
    }
    assert!(
        last_652_before_exit.is_some(),
        "#652: a cleanup instruction must be printed before the legacy path's final exit \"$GATE\""
    );
    assert!(
        last_652_before_exit.unwrap() < final_exit_pos,
        "#652: the legacy-path cleanup print must precede the final exit \"$GATE\""
    );
}

/// Both cleanup print sites must reference the EXACT run-scoped host paths ($STRIH_HOST_PATH /
/// $STREAM_HOST_PATH) — never a directory sweep or wildcard. This is the #649-mirrored safety
/// discipline: never delete anything outside the exact recording THIS run created.
#[test]
fn recording_e2e_sh_cleanup_uses_exact_host_paths_never_a_sweep() {
    let s = read_recording_e2e();
    assert!(
        s.contains("Remove-Item -Force -LiteralPath '${STRIH_HOST_PATH"),
        "#652: cleanup must delete the EXACT strih host path via -LiteralPath (never a glob)"
    );
    assert!(
        s.contains("Remove-Item -Force -LiteralPath '${STREAM_HOST_PATH"),
        "#652: cleanup must delete the EXACT stream host path via -LiteralPath (never a glob)"
    );
    // Never a wildcard sweep alongside the cleanup instructions (e.g. deleting *.mkv or a whole
    // directory) — the issue explicitly calls out that April/May real service recordings must
    // never be touched.
    assert!(
        !s.contains("Remove-Item -Force -Recurse"),
        "#652: cleanup must never recurse/sweep a directory — exact file paths only"
    );
}

/// KEEP_RECORDINGS=1 must be a real, documented opt-out present at BOTH cleanup print sites.
#[test]
fn recording_e2e_sh_cleanup_respects_keep_recordings_opt_out() {
    let s = read_recording_e2e();
    let occurrences = s.matches("KEEP_RECORDINGS:-0").count();
    assert!(
        occurrences >= 2,
        "#652: KEEP_RECORDINGS=1 must be checked at BOTH cleanup print sites (per-box + legacy), \
         found {occurrences} occurrence(s)"
    );
    assert!(
        s.contains("skipping the recording-cleanup plan"),
        "#652: KEEP_RECORDINGS=1 must produce a visible skip message, not a silent no-op"
    );
}
