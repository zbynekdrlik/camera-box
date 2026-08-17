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
//! genlock-jitter-report --file /path/to/strih-obs.log \
//!     --verdict-source 'cg' --verdict-source 'NDI obs hudba'
//! ```
//!
//! `--verdict-source <NAME>` (#811, repeatable): a distinct SCRIPTABLE verdict mode for the
//! resolume-snv (CG box) maintenance check. For each requested input it prints one
//! `RESOLUME-VERDICT <name>: PASS/FAIL/ABSENT` line, evaluating that source's genlock-FIFO
//! window against the frame-loss-free acceptance bounds (`camera_box::resolume_playback`):
//! skew flat within `--skew-bound-ms` (default 20) and ZERO drop/underrun/relock/late-hold/
//! backward-regime deltas over the window, with at least `--min-samples` (default 2) samples.
//! Exits 3 if ANY requested source FAILs or is ABSENT, unless `--verdict-report-only` (then
//! always 0 — telemetry, not a gate). This mode prints only the verdict lines (run with no
//! flag for the full tables) and never touches `--json` (input-side, shape-locked). Used
//! after a dantesync roll onto resolume to confirm frame-loss-free playback of its
//! `RESOLUME-SNV (cg-obs)` feed on strih/stream — see `.claude/skills/ops`.
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
    summarize_send_all, AuditSummary, SendAuditKind, SendAuditSummary,
};
use camera_box::resolume_playback::{evaluate, PlaybackBounds, PlaybackVerdict, PlaybackWindow};
use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json_mode = args.iter().any(|a| a == "--json");

    // #811 — resolume-snv frame-loss-free playback verdict mode. When one or
    // more `--verdict-source <NAME>` are given, this is a distinct SCRIPTABLE
    // path: it emits one `RESOLUME-VERDICT <name>: PASS/FAIL/ABSENT` line per
    // requested input and exits nonzero (3) if ANY requested source FAILs or
    // is ABSENT — unless `--verdict-report-only` forces exit 0. It never
    // prints the big per-source tables (run with no flag for those) and never
    // touches `--json` (input-side, shape-locked). Optional bound overrides:
    // `--skew-bound-ms N` (default 20), `--min-samples N` (default 2).
    let verdict_sources = collect_repeated_flag(&args, "--verdict-source");
    let verdict_report_only = args.iter().any(|a| a == "--verdict-report-only");

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

    // #811 — resolume playback verdict mode (distinct scriptable path; see the
    // flag doc in main's head). Uses only the INPUT-side FIFO summaries.
    if !verdict_sources.is_empty() {
        let bail = |msg: String| -> ! {
            eprintln!("ERROR: {msg}");
            std::process::exit(2);
        };
        let skew_bound_ms = match parse_i64_flag(
            &args,
            "--skew-bound-ms",
            PlaybackBounds::default().skew_bound_ms,
        ) {
            Ok(v) if v >= 0 => v,
            Ok(v) => bail(format!("--skew-bound-ms must be >= 0 (got {v})")),
            Err(e) => bail(e),
        };
        let min_samples = match parse_usize_flag(
            &args,
            "--min-samples",
            PlaybackBounds::default().min_samples,
        ) {
            Ok(v) => v,
            Err(e) => bail(e),
        };
        let bounds = PlaybackBounds {
            skew_bound_ms,
            min_samples,
        };
        let summaries = summarize_all(&samples);
        let code =
            print_resolume_verdicts(&verdict_sources, &summaries, &bounds, verdict_report_only);
        std::process::exit(code);
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

/// Collect every value of a repeatable `--flag VALUE` from `args`, in order.
fn collect_repeated_flag(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// Parse an optional `--flag N` (i64): `default` when absent, the parsed value
/// when present + valid, or `Err(message)` when present-but-unparseable. A
/// malformed bound on a verification gate is a HARD error, never a silent
/// fallback to the default — a silent fallback would mask an operator typo
/// (`--skew-bound-ms 2O`) behind a misleading PASS/FAIL verdict.
fn parse_i64_flag(args: &[String], flag: &str, default: i64) -> Result<i64, String> {
    match collect_repeated_flag(args, flag).last() {
        None => Ok(default),
        Some(v) => v
            .parse::<i64>()
            .map_err(|_| format!("{flag}: '{v}' is not an integer")),
    }
}

/// Parse an optional `--flag N` (usize) with the same absent/valid/error
/// contract as [`parse_i64_flag`].
fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize, String> {
    match collect_repeated_flag(args, flag).last() {
        None => Ok(default),
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| format!("{flag}: '{v}' is not a non-negative integer")),
    }
}

/// #811 — map one INPUT-side [`AuditSummary`] onto the frame-loss verdict's window view.
fn window_from_summary(s: &AuditSummary) -> PlaybackWindow {
    PlaybackWindow {
        source: s.source.clone(),
        samples: s.samples,
        latency_ms: s.latency_ms,
        max_abs_head_skew_ms: s.max_abs_head_skew_ms,
        delta_dropped_due: s.delta_dropped_due,
        delta_underruns: s.delta_underruns,
        delta_relocks: s.delta_relocks,
        delta_late_holds: s.delta_late_holds,
        delta_backward_regime_ticks: s.delta_backward_regime_ticks,
    }
}

/// #811 — emit one `RESOLUME-VERDICT <name>: PASS/FAIL/ABSENT` line per requested source and
/// return the process exit code. A requested source with no matching genlock-fifo audit
/// summary is ABSENT (treated as a failure — the input was never seen, so playback cannot be
/// confirmed frame-loss-free). Returns 0 when every requested source PASSed, or when
/// `report_only` is set (the caller wanted telemetry, not a gate); otherwise 3.
fn print_resolume_verdicts(
    sources: &[String],
    summaries: &[AuditSummary],
    bounds: &PlaybackBounds,
    report_only: bool,
) -> i32 {
    let mut any_bad = false;
    for name in sources {
        match summaries.iter().find(|s| &s.source == name) {
            Some(s) => {
                let PlaybackVerdict { pass, reasons, .. } =
                    evaluate(&window_from_summary(s), bounds);
                if pass {
                    println!(
                        "RESOLUME-VERDICT {name}: PASS ({} samples, latency {} ms, max_skew {} ms)",
                        s.samples, s.latency_ms, s.max_abs_head_skew_ms
                    );
                } else {
                    any_bad = true;
                    println!("RESOLUME-VERDICT {name}: FAIL -- {}", reasons.join("; "));
                }
            }
            None => {
                any_bad = true;
                println!(
                    "RESOLUME-VERDICT {name}: ABSENT -- no 'genlock-fifo audit' lines for this input \
                     (wrong log, input not genlocked, or the source name does not match)"
                );
            }
        }
    }
    if any_bad && !report_only {
        3
    } else {
        0
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

#[cfg(test)]
mod verdict_tests {
    use super::*;

    #[test]
    fn collect_repeated_flag_gathers_all_values_in_order() {
        let args: Vec<String> = [
            "prog",
            "--verdict-source",
            "cg",
            "--json",
            "--verdict-source",
            "NDI obs hudba",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            collect_repeated_flag(&args, "--verdict-source"),
            vec!["cg".to_string(), "NDI obs hudba".to_string()]
        );
    }

    #[test]
    fn collect_repeated_flag_empty_when_absent_or_dangling() {
        let none: Vec<String> = ["prog", "--file", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(collect_repeated_flag(&none, "--verdict-source").is_empty());
        // A trailing flag with no following value yields nothing (no panic).
        let dangling: Vec<String> = ["prog", "--verdict-source"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(collect_repeated_flag(&dangling, "--verdict-source").is_empty());
    }

    #[test]
    fn parse_flags_default_when_absent_valid_when_present_error_on_garbage() {
        let args: Vec<String> = ["prog", "--skew-bound-ms", "35", "--min-samples", "nope"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // present + valid -> the value.
        assert_eq!(parse_i64_flag(&args, "--skew-bound-ms", 20), Ok(35));
        // absent -> the default.
        assert_eq!(parse_i64_flag(&args, "--absent", 20), Ok(20));
        // present-but-garbage -> a hard error (never a silent fall-back to default).
        assert!(parse_usize_flag(&args, "--min-samples", 2).is_err());
    }

    fn summary(source: &str, dropped: u64, skew: i64, samples: usize) -> AuditSummary {
        AuditSummary {
            source: source.to_string(),
            samples,
            latency_ms: 3,
            delta_underruns: 0,
            delta_holds: 4,
            delta_overruns: 2,
            delta_backward_steps: 0,
            delta_backward_regime_ticks: 0,
            delta_dropped_due: dropped,
            delta_relocks: 0,
            delta_late_holds: 0,
            max_abs_head_skew_ms: skew,
            mean_abs_head_skew_ms: 0.0,
            mean_head_skew_ms: 0.0,
            peak_depth: 5,
        }
    }

    // #811 review 🔵-1: exercise window_from_summary + evaluate through the REAL
    // bin path (print_resolume_verdicts) on a POPULATED AuditSummary, so a
    // field-swap typo in window_from_summary can't slip through untested.
    #[test]
    fn present_source_pass_is_0_and_fail_is_3_through_the_bin_path() {
        let want = vec!["cg".to_string()];
        let bounds = PlaybackBounds::default();
        // clean (holds/overruns move but are not gated) -> PASS -> 0
        let clean = vec![summary("cg", 0, 8, 30)];
        assert_eq!(print_resolume_verdicts(&want, &clean, &bounds, false), 0);
        // drops + a skew excursion -> FAIL -> 3
        let bad = vec![summary("cg", 3, 45, 30)];
        assert_eq!(print_resolume_verdicts(&want, &bad, &bounds, false), 3);
        // ...but report-only never gates.
        assert_eq!(print_resolume_verdicts(&want, &bad, &bounds, true), 0);
    }

    #[test]
    fn absent_source_is_a_failure_exit_3_but_report_only_is_0() {
        let summaries: Vec<AuditSummary> = Vec::new();
        let want = vec!["cg".to_string()];
        let bounds = PlaybackBounds::default();
        assert_eq!(
            print_resolume_verdicts(&want, &summaries, &bounds, false),
            3
        );
        assert_eq!(print_resolume_verdicts(&want, &summaries, &bounds, true), 0);
    }

    #[test]
    fn no_requested_sources_is_clean_exit_0() {
        let summaries: Vec<AuditSummary> = Vec::new();
        let bounds = PlaybackBounds::default();
        assert_eq!(print_resolume_verdicts(&[], &summaries, &bounds, false), 0);
    }
}
