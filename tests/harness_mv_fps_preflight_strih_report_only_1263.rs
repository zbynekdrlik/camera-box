//! issue 1263 — the `[4d1/8]` MV-fps floor preflight's **strih** term is REPORT-ONLY while issue
//! 1260 is open; the **imag** term stays STRICT.
//!
//! Root cause (issue 1260): the strih 4K divisor-1 Multiview floor (28 — the issue-776 canvas/2−tol
//! retarget) pre-dates the 2026-08-28 seven-camera fleet reactivation. A healthy-core-loop strih
//! (GetStats: renderTotalFrames delta exactly 30/s, 0 render skips) now idles the 4K composite MV at
//! 13–29 fps (fresh instances 13–16), so once issue 1261 made the `[4d1/8]` gate actually decide the
//! strih floor term deterministically aborted every run (three live aborts the same day). The
//! verdict never reads strih MV fps (monitoring surface only). So while issue 1260 (the MV perf
//! defect) is open, a CONFIRMED strih collapse is REPORT-ONLY — a loud `WARNING (issue 1260)` naming
//! the measured line, never an `exit 1`. A CONFIRMED imag collapse still aborts (imag holds its
//! floor reliably; its render-health preflight gates it elsewhere too). issue 1263 is the walk-back
//! tracker: flip strih back to strict in the PR that closes issue 1260.
//!
//! Same offline pattern as `tests/harness_mv_fps_preflight_1091.rs`: source the REAL source-only lib
//! under the caller's `set -euo pipefail`, drive the assert with the `MV_FPS_PREFLIGHT_PROBE_CMD`
//! fake-probe seam + a fake gate binary — no ssh, no Rust binary, no rig. RED before the lib split;
//! GREEN after.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/mv-fps-preflight.sh");
    assert!(s.exists(), "{} not found (issue 1263)", s.display());
    s
}

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

/// A fake gate: consumes stdin, picks the Nth exit from space-separated $FAKE_GATE_EXITS (clamped to
/// the last) via a per-test $FAKE_GATE_COUNTER file, and emits a `FAIL …` line on exit 1 (what the
/// real gate prints) so the assert's detail extraction is exercised too. Same shape as the 1091
/// harness's fake gate.
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
[ "$ec" = "1" ] && echo "FAIL monitor=1 divisor=1 rendered_fps=14.0 < floor=28.0 (target 30, 3840x2160)"
exit "$ec"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

const AUDIT_LINE: &str = "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=14.0 target=30 floor=28.0 cx=3840 cy=2160";

/// Run `mv_fps_preflight_assert` for ONE box (`box_spec` = "name|ip|os|user|pw") under
/// `set -euo pipefail` with a fake probe + fake gate. A trailing `echo PROCEEDED` proves the assert
/// RETURNED (did not `exit 1`). Returns (exit, stdout, stderr).
fn run_assert(box_spec: &str, probe_out: &str, gate_exits: &str) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let probe = write_fake_probe(dir.path());
    let gate = write_fake_gate(dir.path());
    let counter = dir.path().join("gate.counter");
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\nmv_fps_preflight_assert \"$GATE\" \"$BOX\"\necho PROCEEDED")
        .env("LIB", lib_path())
        .env("GATE", &gate)
        .env("BOX", box_spec)
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

/// Source the lib under `set -euo pipefail`, run `body`, return its trimmed stdout (asserts rc 0).
fn lib_stdout(body: &str) -> String {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .current_dir(manifest_dir())
        .output()
        .expect("run lib harness");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "body failed: {body}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// -------------------------------------------------------------------------------------------
// the pure per-box report-only predicate — strih report-only, everything else strict
// -------------------------------------------------------------------------------------------
#[test]
fn report_only_predicate_strih_yes_imag_no_1263() {
    // strih -> report-only (exit 0)
    assert_eq!(
        lib_stdout("mv_fps_preflight_term_is_report_only strih && echo RO || echo STRICT"),
        "RO",
        "the strih term must be REPORT-ONLY while issue 1260 is open"
    );
    // imag -> strict (exit 1)
    assert_eq!(
        lib_stdout("mv_fps_preflight_term_is_report_only imag && echo RO || echo STRICT"),
        "STRICT",
        "the imag term must stay STRICT"
    );
    // any other box name -> strict (fail-safe: a new box is strict unless explicitly report-only)
    assert_eq!(
        lib_stdout("mv_fps_preflight_term_is_report_only stream && echo RO || echo STRICT"),
        "STRICT",
        "an unlisted box must default to STRICT"
    );
}

// -------------------------------------------------------------------------------------------
// strih: a CONFIRMED sustained collapse is REPORT-ONLY (loud WARN, never abort) while 1260 open
// -------------------------------------------------------------------------------------------
#[test]
fn strih_sustained_collapse_is_report_only_and_does_not_abort_1263() {
    // Below floor on BOTH the first read and the grace re-read (a real sustained collapse) — but on
    // strih, while issue 1260 is open, this must NOT abort: proceed with a loud issue-1260 WARN.
    let (rc, out, err) = run_assert("strih|10.0.0.1|win|u|p", AUDIT_LINE, "1 1");
    assert_eq!(
        rc, 0,
        "a CONFIRMED strih collapse must be REPORT-ONLY (no abort) while issue 1260 is open:\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("PROCEEDED"),
        "strih report-only must let the run proceed:\n{out}"
    );
    assert!(
        err.contains("WARNING (issue 1260)"),
        "strih report-only must print the loud issue-1260 WARN:\n{err}"
    );
    assert!(
        err.contains("REPORT-ONLY"),
        "the strih WARN must say REPORT-ONLY:\n{err}"
    );
    assert!(
        err.contains("14.0"),
        "the strih WARN must name the measured line (rendered_fps):\n{err}"
    );
    assert!(
        !err.contains("CONFIRMED below its floor"),
        "strih must NOT hit the abort path while issue 1260 is open:\n{err}"
    );
}

// -------------------------------------------------------------------------------------------
// imag: a CONFIRMED sustained collapse still ABORTS (the strict term is unchanged)
// -------------------------------------------------------------------------------------------
#[test]
fn imag_sustained_collapse_still_aborts_1263() {
    let (rc, out, err) = run_assert("imag|10.0.0.2|linux|u|p", AUDIT_LINE, "1 1");
    assert_eq!(
        rc, 1,
        "a CONFIRMED imag collapse must still ABORT (imag stays STRICT):\nstdout={out}\nstderr={err}"
    );
    assert!(
        !out.contains("PROCEEDED"),
        "the run must NOT proceed past a confirmed imag collapse:\n{out}"
    );
    assert!(
        err.contains("CONFIRMED below its floor"),
        "the imag abort must name the confirmed below-floor collapse:\n{err}"
    );
    assert!(
        err.contains("imag"),
        "the imag abort must name the collapsed box:\n{err}"
    );
    assert!(
        !err.contains("WARNING (issue 1260)"),
        "imag must NOT get the strih report-only WARN:\n{err}"
    );
}
