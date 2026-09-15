//! issue 1040 (harness item) — the `[4d1/8]` MV-fps preflight's STRICT (imag) term must SETTLE on
//! FRESH `multiview-audit` samples after a BELOW first read, instead of a one-shot 6 s grace re-read
//! of a window whose MEDIAN can straddle a `[4d0/8]`-cleared 25 W PL1 clamp episode.
//!
//! Root cause: `mv-fps-gate` classifies each projector's WINDOW MEDIAN `rendered_fps`
//! (`median_recent_rendered_fps`, `src/mv_audit.rs`), so right after the `[4d0/8]` step-down
//! pre-gate waited out a 62 s clamp the recent window still carries clamp-era samples — run
//! 34986461596 aborted with `median_fps=23.0 < floor=28.0 … latest rendered_fps 30.0`: the LATEST
//! sample was already at target, the box had recovered, but the median hadn't. The 6 s grace re-read
//! re-reads the SAME window and re-runs the SAME median gate, so it cannot rescue this shape. The fix
//! polls for FRESH samples (anchored on each `multiview-audit` line's own identity — never a
//! wall-clock divisor, the issue-797 lesson) and decides from the individual fresh samples:
//! N (default 3) consecutive fresh ≥ floor → recovered → proceed; any fresh sample < floor →
//! collapse CONFIRMED → abort (exit 1, exactly as today); < N fresh within a bounded budget, or an
//! unreadable re-read → UNKNOWN → report-only NOTE (NEVER false-abort a CI gate — the user's hardest
//! constraint). The strih report-only term (issue 1260) keeps its own grace path, untouched.
//!
//! Same offline convention as `tests/harness_mv_fps_preflight_1091.rs`: source the REAL source-only
//! lib under the caller's `set -euo pipefail`, drive the pure functions + the assert with a fake
//! SEQUENCE probe (`MV_FPS_PREFLIGHT_PROBE_CMD`) + a fake gate + injected sleep/clock seams — no ssh,
//! no Rust binary, no rig, no real waiting. CI is the first compile; RED before the settle lands,
//! GREEN after.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/mv-fps-preflight.sh");
    assert!(s.exists(), "{} not found (issue 1040)", s.display());
    s
}

/// Source the REAL lib under `set -euo pipefail` and run `body`. Returns (exit, stdout, stderr).
fn run_lib(body: &str) -> (i32, String, String) {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lib_stdout(body: &str) -> String {
    let (rc, out, err) = run_lib(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// -------------------------------------------------------------------------------------------
// lib shape — the new pure functions exist
// -------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_settle_functions_1040() {
    for f in [
        "mv_fps_preflight_latest_sample",
        "mv_fps_preflight_sample_verdict",
        "_mv_fps_preflight_settle_now",
        "mv_fps_preflight_settle_strict",
    ] {
        let out = lib_stdout(&format!("type {f} >/dev/null 2>&1 && echo OK"));
        assert_eq!(
            out, "OK",
            "{f} is not defined by the preflight lib (issue 1040)"
        );
    }
}

// -------------------------------------------------------------------------------------------
// pure: latest_sample extracts the NEWEST audit line's id + rendered_fps + floor
// -------------------------------------------------------------------------------------------
#[test]
fn latest_sample_extracts_newest_id_fps_floor_1040() {
    // Two audit lines; the NEWEST (last) is the one whose fields are extracted. The id is the whole
    // line (its own identity — the timestamp prefix advances every emit), fps/floor its fields.
    let body = "printf '%s\\n' \
        '20:15:03.000: multiview-audit: monitor=8 divisor=2 rendered_fps=12.5 target=30 floor=28.0 cx=2880 cy=1800' \
        '20:15:08.000: multiview-audit: monitor=8 divisor=2 rendered_fps=30.00 target=30 floor=28.0 cx=2880 cy=1800' \
        | mv_fps_preflight_latest_sample";
    let out = lib_stdout(body);
    let fields: Vec<&str> = out.split('\t').collect();
    assert_eq!(
        fields.len(),
        3,
        "expected id<TAB>fps<TAB>floor, got: {out:?}"
    );
    assert!(
        fields[0].contains("20:15:08.000") && fields[0].contains("rendered_fps=30.00"),
        "id must be the NEWEST line verbatim: {:?}",
        fields[0]
    );
    assert_eq!(fields[1], "30.00", "rendered_fps of the newest line");
    assert_eq!(fields[2], "28.0", "floor of the newest line");

    // No audit line -> empty output (the caller then treats it as "no fresh sample").
    assert_eq!(
        lib_stdout("printf 'some other log line\\n' | mv_fps_preflight_latest_sample"),
        "",
        "a tail with no multiview-audit line yields no sample"
    );
}

// -------------------------------------------------------------------------------------------
// pure: sample_verdict — ok / below / bad
// -------------------------------------------------------------------------------------------
#[test]
fn sample_verdict_ok_below_bad_1040() {
    assert_eq!(
        lib_stdout("mv_fps_preflight_sample_verdict 30.0 28.0"),
        "ok"
    );
    assert_eq!(
        lib_stdout("mv_fps_preflight_sample_verdict 28.0 28.0"),
        "ok",
        ">= floor is ok"
    );
    assert_eq!(
        lib_stdout("mv_fps_preflight_sample_verdict 23.0 28.0"),
        "below"
    );
    assert_eq!(
        lib_stdout("mv_fps_preflight_sample_verdict 9.0 28.0"),
        "below"
    );
    assert_eq!(
        lib_stdout("mv_fps_preflight_sample_verdict '' 28.0"),
        "bad",
        "missing fps"
    );
    assert_eq!(
        lib_stdout("mv_fps_preflight_sample_verdict 30.0 xx"),
        "bad",
        "non-numeric floor"
    );
}

// -------------------------------------------------------------------------------------------
// the assert's STRICT settle path — fake SEQUENCE probe + fake gate, injected sleep/clock
// -------------------------------------------------------------------------------------------

/// A fake probe that emits the Nth (0-based) `@@@`-delimited chunk of $SEQ_FILE per invocation,
/// tracked by a $SEQ_COUNTER file — so ONE binary replays a scripted sequence of OBS-log reads
/// (each chunk = the multiview-audit lines that read would return; an empty chunk = an unreadable
/// read). Once past the last chunk it repeats the last one.
fn write_seq_probe(dir: &Path) -> PathBuf {
    let p = dir.join("seq-probe.sh");
    std::fs::write(
        &p,
        r#"#!/usr/bin/env bash
cf="${SEQ_COUNTER:?}"; sf="${SEQ_FILE:?}"
n="$(cat "$cf" 2>/dev/null || true)"; case "$n" in ''|*[!0-9]*) n=0 ;; esac
echo $((n + 1)) > "$cf"
awk -v want="$n" '
  BEGIN { idx = 0 }
  /^@@@$/ { idx++; next }
  { rows[idx] = rows[idx] $0 "\n" }
  END {
    # idx now equals the number of @@@ separators = the LAST chunk index (empty trailing chunks
    # included), so an empty chunk is reachable + repeated -- this exercises the unreadable-read
    # path, distinct from a repeated STALE line (a single-chunk sequence).
    k = (want <= idx ? want : idx)
    if (k in rows) printf "%s", rows[k]
  }' "$sf"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

/// A fake gate that ALWAYS reports BELOW (exit 1) with a `FAIL …` line — the strict box's first
/// read must be BELOW for the settle to engage; after that the settle decides from FRESH samples
/// (parsed directly), never by re-running the gate, so a single "always 1" gate is enough.
fn write_below_gate(dir: &Path) -> PathBuf {
    let p = dir.join("below-gate.sh");
    std::fs::write(
        &p,
        "#!/usr/bin/env bash\ncat >/dev/null 2>&1\n\
         echo \"FAIL monitor=8 divisor=2 median_fps=23.0 < floor=28.0 over 9 sample(s) (target 30, 2880x1800, latest rendered_fps 30.0)\"\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

fn audit(ts: &str, fps: &str) -> String {
    format!("{ts}: multiview-audit: monitor=8 divisor=2 rendered_fps={fps} target=30 floor=28.0 cx=2880 cy=1800")
}

/// Run `mv_fps_preflight_assert` for the imag box over a scripted probe `sequence` (each element =
/// one read's audit lines) under `set -euo pipefail`, with the settle sleep no-op'd and a small
/// budget. A trailing `echo PROCEEDED` proves the assert RETURNED (did not `exit 1`).
fn run_imag_settle(sequence: &[&str], budget_s: &str, n: &str) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let probe = write_seq_probe(dir.path());
    let gate = write_below_gate(dir.path());
    let seq_file = dir.path().join("seq.txt");
    std::fs::write(&seq_file, sequence.join("\n@@@\n")).unwrap();
    let counter = dir.path().join("seq.counter");
    std::fs::write(&counter, "0").unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\nmv_fps_preflight_assert \"$GATE\" \"imag|10.0.0.2|linux|u|p\"\necho PROCEEDED")
        .env("LIB", lib_path())
        .env("GATE", &gate)
        .env("MV_FPS_PREFLIGHT_PROBE_CMD", &probe)
        .env("SEQ_FILE", &seq_file)
        .env("SEQ_COUNTER", &counter)
        .env("MV_FPS_PREFLIGHT_SETTLE_SLEEP_CMD", ":")
        .env("MV_FPS_PREFLIGHT_SETTLE_POLL", "6")
        .env("MV_FPS_PREFLIGHT_SETTLE_N", n)
        .env("MV_FPS_PREFLIGHT_SETTLE_S", budget_s)
        .current_dir(manifest_dir())
        .output()
        .expect("run imag settle harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn fresh_below_sample_confirms_the_collapse_and_aborts_1040() {
    // The first read is BELOW (median). The settle then sees a FRESH below-floor sample (distinct
    // line identity, rendered_fps 9.0 < floor 28.0) -> collapse CONFIRMED on fresh data -> abort.
    let seq = [
        audit("20:15:03.000", "9.0"),
        audit("20:15:09.000", "9.0"),
        audit("20:15:15.000", "9.0"),
    ];
    let seq: Vec<&str> = seq.iter().map(String::as_str).collect();
    let (rc, out, err) = run_imag_settle(&seq, "60", "3");
    assert_eq!(
        rc, 1,
        "a fresh below-floor sample must confirm + abort:\nstdout={out}\nstderr={err}"
    );
    assert!(
        !out.contains("PROCEEDED"),
        "must NOT proceed past a confirmed collapse:\n{out}"
    );
    assert!(
        err.contains("CONFIRMED on fresh data"),
        "the settle must confirm on fresh data:\n{err}"
    );
    assert!(
        err.contains("CONFIRMED below its floor"),
        "the abort must name the confirmed collapse:\n{err}"
    );
    assert!(err.contains("imag"), "the abort must name the box:\n{err}");
}

#[test]
fn stale_below_then_fresh_above_recovers_and_proceeds_1040() {
    // The straddling-median incident: the first read is BELOW (median), the very next read still
    // shows the STALE baseline line (== the below read's newest — not counted), then FRESH samples
    // at/above floor arrive. N=3 consecutive fresh >= floor -> recovered -> proceed. This is the
    // false-abort the 6 s grace re-read caused and the settle fixes.
    let seq = [
        audit("20:15:03.000", "9.0"),   // first read (baseline, BELOW median)
        audit("20:15:03.000", "9.0"),   // STALE re-read of the same newest line (not fresh)
        audit("20:15:09.000", "30.00"), // fresh #1 >= floor
        audit("20:15:15.000", "30.00"), // fresh #2 >= floor
        audit("20:15:21.000", "30.00"), // fresh #3 >= floor -> recovered
    ];
    let seq: Vec<&str> = seq.iter().map(String::as_str).collect();
    let (rc, out, err) = run_imag_settle(&seq, "120", "3");
    assert_eq!(
        rc, 0,
        "a recovered box must NOT abort:\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("PROCEEDED"),
        "a recovered box must proceed:\n{out}"
    );
    assert!(
        err.contains("recovered on fresh samples") && err.contains("3/3"),
        "the settle must log recovery on 3/3 fresh >= floor:\n{err}"
    );
    assert!(
        !err.contains("CONFIRMED below its floor"),
        "a recovered box must NOT hit the abort path (the false-abort this fixes):\n{err}"
    );
}

#[test]
fn no_new_lines_is_inconclusive_report_only_never_aborts_1040() {
    // The box only ever re-emits the SAME baseline line (no new samples) — the settle can collect no
    // FRESH sample, so within the bounded budget it is UNKNOWN -> a report-only NOTE, the run
    // proceeds. Never false-abort a CI gate on inconclusive data (the live issue-1083 watchdog owns a
    // genuinely sustained collapse).
    let seq = [audit("20:15:03.000", "9.0")]; // one chunk, repeated forever by the probe
    let (rc, out, err) = run_imag_settle(&seq, "20", "3");
    assert_eq!(
        rc, 0,
        "inconclusive (no fresh samples) must NOT abort:\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("PROCEEDED"),
        "an inconclusive settle must proceed:\n{out}"
    );
    assert!(
        err.contains("inconclusive, proceeding report-only"),
        "the settle must log the inconclusive report-only NOTE:\n{err}"
    );
    assert!(
        !err.contains("CONFIRMED below its floor"),
        "inconclusive data must never reach the abort path:\n{err}"
    );
}

#[test]
fn unreadable_reread_never_aborts_1040() {
    // The first read is BELOW, then every settle re-read is UNREADABLE (empty) — no fresh sample can
    // ever be collected, so within the budget it is UNKNOWN -> report-only NOTE, proceed. An
    // unreadable box must never false-abort the whole E2E (the grace path's own guarantee, kept).
    let seq = [audit("20:15:03.000", "9.0"), ""]; // baseline, then empty reads forever
    let seq: Vec<&str> = seq.iter().copied().collect();
    let (rc, out, err) = run_imag_settle(&seq, "20", "3");
    assert_eq!(
        rc, 0,
        "an unreadable re-read must NOT abort:\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("PROCEEDED"),
        "an unreadable settle must proceed:\n{out}"
    );
    assert!(
        err.contains("inconclusive, proceeding report-only"),
        "an unreadable re-read must end in the inconclusive report-only NOTE:\n{err}"
    );
}
