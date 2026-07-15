//! #272 — genlock-jitter-report CLI binary (default features, no probe deps).
//!
//! Reads OBS log text (the `genlock-fifo audit` lines emitted every ~5s by the vendored
//! genlock FIFO — `vendor/obs-studio/libobs/obs-source.c` `genlock_audit_log`) from stdin
//! or `--file <path>`, parses every audit line via `camera_box::jitter_audit`, and prints
//! a PER-SOURCE report: sample count, the effective `latency_ms` this window was captured
//! at, the DELTA (last-minus-first) loss/backpressure counters over the window, and the
//! per-tick presentation-skew jitter (max/mean `|ts_head_skew_ms|`).
//!
//! This is the measurement half of the #272 reserve_ms-floor investigation: it turns a
//! captured OBS log segment into the "did lowering the reserve introduce loss, how big is
//! the real arrival jitter" answer. Building a floor-varied OBS binary, deploying it, and
//! capturing a recording window's log on the live rig per reserve_ms candidate is a
//! SEPARATE, supervisor-driven step (a build-matrix change, out of scope here) — run this
//! binary once per captured segment to get that segment's summary. See
//! `docs/genlock-latency-floor-rationale.md` for the full runbook.
//!
//! **Usage:**
//! ```text
//! genlock-jitter-report < obs.log
//! genlock-jitter-report --file /path/to/obs.log
//! genlock-jitter-report --file /path/to/obs.log --json
//! ```
//!
//! `--json` (#757): prints [`camera_box::jitter_audit::summaries_to_json`]'s per-source
//! object instead of the text table — the machine-readable shape a pre-record phase
//! calibrator (`scripts/prerecord_phase_calibrate.py`) consumes to reconstruct each source's
//! absolute cam→strih transit latency (`latency_ms + mean_head_skew_ms`) without a full
//! recording. The text table stays the default (unchanged) for a human reading it directly.
//!
//! **Exit codes:** `0` — at least one source's audit lines were found and reported;
//! `2` — no `genlock-fifo audit` lines found in the input, or an I/O error reading it.

use camera_box::jitter_audit::{parse_audit_lines, summaries_to_json, summarize_all};
use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json_mode = args.iter().any(|a| a == "--json");

    let text = match read_input() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(2);
        }
    };

    let samples = parse_audit_lines(&text);
    if samples.is_empty() {
        eprintln!(
            "ERROR: no 'genlock-fifo audit' lines found in the input \
             (wrong log file, or this OBS build isn't genlocked/logging yet)"
        );
        std::process::exit(2);
    }

    let summaries = summarize_all(&samples);

    if json_mode {
        println!("{}", summaries_to_json(&summaries));
        return;
    }

    println!(
        "{:<20} {:>8} {:>11} {:>11} {:>7} {:>14} {:>8} {:>9} {:>16} {:>17} {:>11}",
        "source",
        "samples",
        "latency_ms",
        "d_underrun",
        "d_hold",
        "d_dropped_due",
        "d_relock",
        "d_latehold",
        "max_abs_skew_ms",
        "mean_abs_skew_ms",
        "peak_depth"
    );
    for s in &summaries {
        println!(
            "{:<20} {:>8} {:>11} {:>11} {:>7} {:>14} {:>8} {:>9} {:>16} {:>17.2} {:>11}",
            s.source,
            s.samples,
            s.latency_ms,
            s.delta_underruns,
            s.delta_holds,
            s.delta_dropped_due,
            s.delta_relocks,
            s.delta_late_holds,
            s.max_abs_head_skew_ms,
            s.mean_abs_head_skew_ms,
            s.peak_depth
        );
    }
}

/// Read the input text: `--file <path>` if given, else stdin.
///
/// #757: LOSSY UTF-8 decode (`String::from_utf8_lossy`), never `read_to_string`'s strict
/// decode. A real strih OBS log pulled via PowerShell `Get-Content` over ssh can carry a
/// handful of invalid UTF-8 bytes from a console-encoding hop (the multi-byte "≈" glyph in
/// `latency_ms=N (≈F frames @ ...)` is the observed offender) — a STRICT read fails on the
/// very first bad byte anywhere in a 600KB+ log, discarding every otherwise-parseable audit
/// line in the whole file. A lossy read changes nothing for a genuinely clean line: the
/// corrupted bytes only ever land inside decorative text `parse_audit_line` already treats
/// as skippable (no `key=value` token can contain a raw non-ASCII byte in the first place).
fn read_input() -> std::io::Result<String> {
    let args: Vec<String> = std::env::args().collect();
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        if arg == "--file" {
            if let Some(path) = it.next() {
                let bytes = std::fs::read(path)?;
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    }
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
