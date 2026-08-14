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
//! #874: the same log also carries the SEND-side audit lines the genlock build emits
//! (`genlock-ndi-output audit '<name>':` / `genlock-ndi-filter audit '<ndi name>':`, from
//! `vendor/distroav/src/ndi-output.cpp` / `ndi-filter.cpp`). In text mode this binary now
//! ALSO prints a per-sender send-side table below the source table when those lines are
//! present: the window DELTAS `d_offered`/`d_sent`/`d_dropped` plus `d_send_wait_ms` — the
//! issue-707 discriminator (large send-wait + drops = the async send is blocking on the
//! receiver; near-zero send-wait + drops = frames never reached the send, fault is upstream
//! in libobs). A log may carry input lines, send lines, or both.
//!
//! `--json` (#757): prints [`camera_box::jitter_audit::summaries_to_json`]'s per-source
//! object instead of the text table — the machine-readable shape a pre-record phase
//! calibrator (`scripts/prerecord_phase_calibrate.py`) consumes to reconstruct each source's
//! absolute cam→strih transit latency (`latency_ms + mean_head_skew_ms`) without a full
//! recording. The text table stays the default (unchanged) for a human reading it directly.
//! `--json` is deliberately INPUT-side only — its shape must not gain keys — so it still
//! requires at least one input audit line.
//!
//! **Exit codes:** `0` — at least one input OR #874 send-side audit line was found and
//! reported; `2` — no audit lines of either kind found in the input, or an I/O error.

use camera_box::jitter_audit::{
    parse_audit_lines, parse_send_audit_lines, summaries_to_json, summarize_all,
    summarize_send_all, SendAuditKind, SendAuditSummary,
};
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

    // --json (#757) is INPUT-side only, deliberately unchanged: the per-source object
    // scripts/prerecord_phase_calibrate.py consumes must not gain keys. Parse + return
    // here BEFORE any send-side work, so the json path never double-parses the log.
    if json_mode {
        if samples.is_empty() {
            eprintln!(
                "ERROR: no 'genlock-fifo audit' lines found in the input \
                 (wrong log file, or this OBS build isn't genlocked/logging yet; \
                 note --json is input-side only -- the #874 send-side lines are reported \
                 in text mode)"
            );
            std::process::exit(2);
        }
        println!("{}", summaries_to_json(&summarize_all(&samples)));
        return;
    }

    // #874: the genlock build also emits send-side audit lines (genlock-ndi-output /
    // genlock-ndi-filter), surfaced in text mode only. Parse them independently over the
    // same log. Text mode reports whichever kinds are present: input FIFO lines, send-side
    // lines, or both.
    let send_samples = parse_send_audit_lines(&text);
    if samples.is_empty() && send_samples.is_empty() {
        eprintln!(
            "ERROR: no 'genlock-fifo audit' or 'genlock-ndi-output/filter audit' lines found \
             in the input (wrong log file, or this OBS build isn't genlocked/logging yet)"
        );
        std::process::exit(2);
    }

    if !samples.is_empty() {
        let summaries = summarize_all(&samples);
        println!(
            "{:<20} {:>8} {:>11} {:>11} {:>7} {:>14} {:>8} {:>9} {:>13} {:>16} {:>17} {:>11}",
            "source",
            "samples",
            "latency_ms",
            "d_underrun",
            "d_hold",
            "d_dropped_due",
            "d_relock",
            "d_latehold",
            // #1009: window delta of the backward-step re-anchor TICK counter — any movement
            // means the configured hold was bypassed during the window (healthy runs: 0).
            "d_regimetick",
            "max_abs_skew_ms",
            "mean_abs_skew_ms",
            "peak_depth"
        );
        for s in &summaries {
            println!(
                "{:<20} {:>8} {:>11} {:>11} {:>7} {:>14} {:>8} {:>9} {:>13} {:>16} {:>17.2} {:>11}",
                s.source,
                s.samples,
                s.latency_ms,
                s.delta_underruns,
                s.delta_holds,
                s.delta_dropped_due,
                s.delta_relocks,
                s.delta_late_holds,
                s.delta_backward_regime_ticks,
                s.max_abs_head_skew_ms,
                s.mean_abs_head_skew_ms,
                s.peak_depth
            );
        }
    }

    if !send_samples.is_empty() {
        // Blank line separates the two tables when both are present.
        if !samples.is_empty() {
            println!();
        }
        print_send_table(&summarize_send_all(&send_samples));
    }
}

/// #874 — print the per-sender send-side report. The load-bearing columns are the window
/// DELTAS: `d_dropped` (frames offered-but-not-sent during the window) alongside
/// `d_send_wait_ms` (send-call block time accrued during the window) — a large `d_send_wait_ms`
/// with `d_dropped > 0` means the send is blocking (receiver/transport backpressure); a
/// near-zero `d_send_wait_ms` with `d_dropped > 0` moves the fault upstream into libobs's
/// output path. The mutex columns are filter-only (`-` for an output row).
fn print_send_table(summaries: &[SendAuditSummary]) {
    println!(
        "{:<7} {:<16} {:>8} {:>10} {:>8} {:>10} {:>15} {:>17} {:>15} {:>17}",
        "kind",
        "name",
        "samples",
        "d_offered",
        "d_sent",
        "d_dropped",
        "d_send_wait_ms",
        "max_send_wait_ms",
        "d_mutex_wait_ms",
        "max_mutex_wait_ms"
    );
    let fmt_opt = |v: Option<f64>| {
        v.map(|x| format!("{x:.3}"))
            .unwrap_or_else(|| "-".to_string())
    };
    for s in summaries {
        let kind = match s.kind {
            SendAuditKind::Output => "output",
            SendAuditKind::Filter => "filter",
        };
        println!(
            "{:<7} {:<16} {:>8} {:>10} {:>8} {:>10} {:>15.3} {:>17.3} {:>15} {:>17}",
            kind,
            s.name,
            s.samples,
            s.delta_offered,
            s.delta_sent,
            s.delta_dropped,
            s.delta_send_wait_ms,
            s.max_send_wait_ms,
            fmt_opt(s.delta_mutex_wait_ms),
            fmt_opt(s.max_mutex_wait_ms)
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
