//! #272 — genlock arrival-jitter audit-log parser + per-run reserve→loss summarizer.
//!
//! The #148 periodic audit line (`vendor/obs-studio/libobs/obs-source.c`
//! `genlock_audit_log`, emitted every ~5s per genlocked source) already carries every
//! signal #272 needs to answer "is the 3ms `GENLOCK_LATENCY_MS_MIN` floor a real jitter
//! floor, or an arbitrary margin?": the cumulative FIFO health counters
//! (`underruns`/`holds`/`dropped_due`/`late_holds`/`overruns`/...) and the per-tick
//! presentation skew (`ts_head_skew_ms` — how far the released frame's timestamp sat from
//! the wall-clock release deadline). This module is the PURE, cross-platform kernel that
//! turns a captured OBS log (any number of `genlock-fifo audit` lines, any number of
//! sources interleaved) into, per source, the DELTA loss/backpressure counters over the
//! captured window plus the head-skew jitter distribution — i.e. "did lowering the
//! reserve introduce loss, and how big is the real arrival jitter".
//!
//! This is the "per-run reserve→loss collector" from the #272 investigation plan: run it
//! once per captured segment (one segment = one reserve_ms floor a differently-built OBS
//! was recording at) and compare the summaries across segments. Building + deploying a
//! floor-varied OBS binary and capturing each segment's log on the live rig is a SEPARATE,
//! supervisor-driven step (see `docs/genlock-latency-floor-rationale.md`); this module (and
//! its thin CLI wrapper `src/bin/genlock-jitter-report.rs`) only analyzes whatever log each
//! run produces. No probe deps, so it unit-tests Tier-0 (default features, no OBS/rig
//! needed) — the same pure-kernel-plus-thin-glue pattern as `reannounce` / `colour_scale`.

use std::collections::HashMap;

/// One parsed `genlock-fifo audit` sample for one source at one point in time.
///
/// Field names mirror the log's `key=value` tokens exactly (see [`parse_audit_line`]).
/// The counters (`received`, `consumed`, `underruns`, `holds`, `overruns`,
/// `backward_steps`, `backward_regime_ticks`, `dropped_due`, `relocks`, `late_holds`,
/// `empty_run`) are CUMULATIVE
/// since the source was created — they only ever increase — so a per-run answer needs the
/// DELTA between the first and last sample of a captured window ([`summarize`]), never the
/// raw value alone. `ts_head_skew_ms` is an instantaneous per-tick value, not cumulative.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditSample {
    /// The OBS source name (the quoted `'%s'` in the log line), e.g. `"NDI cam1"`.
    pub source: String,
    pub received: u64,
    pub consumed: u64,
    pub underruns: u64,
    pub holds: u64,
    pub overruns: u64,
    pub backward_steps: u64,
    /// Frames the release DISCARDED (the #401 honest loss counter) — the primary
    /// "did we lose distinct frames at this reserve" signal.
    pub dropped_due: u64,
    pub relocks: u64,
    /// Boundary matured but the frame never arrived (upstream late/lost) — distinct
    /// from the benign source-early `holds`.
    pub late_holds: u64,
    pub locked: bool,
    pub depth: u64,
    pub peak: u32,
    /// The EFFECTIVE held latency (ms) this sample was captured at.
    pub latency_ms: u32,
    pub src_latency_ms: u32,
    pub global_latency_ms: u32,
    pub preload: u32,
    pub reserve_ms: u32,
    pub cap: u64,
    pub empty_run: u32,
    pub ts_present: u64,
    pub ts_due: u32,
    /// Per-tick skew (ms) between the presented frame's timestamp and the release
    /// deadline — the ARRIVAL-JITTER signal #272 asks to measure against the reserve.
    pub ts_head_skew_ms: i64,
    /// #1009 — CUMULATIVE backward-step re-anchor TICKS (`backward_steps` counts EVENTS;
    /// this counts every re-anchored tick, so a sustained hold-bypass regime is visible as
    /// a climbing rate). Healthy operation keeps it at 0; the E2E/drift gates assert the
    /// window DELTA stays 0. Absent on pre-#1009 logs — parses as 0.
    pub backward_regime_ticks: u64,
}

/// Parse ONE `genlock-fifo audit` log line into an [`AuditSample`].
///
/// Returns `None` if the line does not contain a `genlock-fifo audit '<source>':` marker
/// (i.e. it is not an audit line at all — any other log noise is safely ignored). Any log
/// timestamp/level prefix before the marker is ignored, so this works whether the line
/// came straight from OBS's `blog()` or through an SSH-captured/journald-wrapped log.
///
/// The scan is whitespace-token-based: every `key=value` token in the line (in any order,
/// wherever it sits) is matched against the known field names and parsed; unrecognized
/// tokens (the `'%s':` name-quote fragments, the `(≈N frames @ F fps)` / `(=N ms)` /
/// `(re-arm@N)` / `(#70/#97/...)` decorations from `genlock_audit_log`'s format string)
/// contain no `=` — or an `=` that matches no known key — and are simply skipped. This
/// means the parser never needs to hand-model the exact decoration syntax: it just reads
/// the `key=value` tokens the log line actually carries.
pub fn parse_audit_line(line: &str) -> Option<AuditSample> {
    const MARK: &str = "genlock-fifo audit '";
    let mark_at = line.find(MARK)?;
    let after_mark = &line[mark_at + MARK.len()..];
    let quote_end = after_mark.find('\'')?;
    let source = after_mark[..quote_end].to_string();

    let mut s = AuditSample {
        source,
        ..AuditSample::default()
    };
    let mut saw_any_field = false;

    for tok in line[mark_at..].split_whitespace() {
        let Some(eq) = tok.find('=') else {
            continue;
        };
        let key = &tok[..eq];
        let val = &tok[eq + 1..];
        macro_rules! set {
            ($field:ident) => {
                if let Ok(v) = val.parse() {
                    s.$field = v;
                    saw_any_field = true;
                }
            };
        }
        match key {
            "received" => set!(received),
            "consumed" => set!(consumed),
            "underruns" => set!(underruns),
            "holds" => set!(holds),
            "overruns" => set!(overruns),
            "backward_steps" => set!(backward_steps),
            "dropped_due" => set!(dropped_due),
            "relocks" => set!(relocks),
            "late_holds" => set!(late_holds),
            "locked" => {
                if let Ok(v) = val.parse::<i32>() {
                    s.locked = v != 0;
                    saw_any_field = true;
                }
            }
            "depth" => set!(depth),
            "peak" => set!(peak),
            "latency_ms" => set!(latency_ms),
            "src_latency_ms" => set!(src_latency_ms),
            "global_latency_ms" => set!(global_latency_ms),
            "preload" => set!(preload),
            "reserve_ms" => set!(reserve_ms),
            "cap" => set!(cap),
            "empty_run" => set!(empty_run),
            "ts_present" => set!(ts_present),
            "ts_due" => set!(ts_due),
            "ts_head_skew_ms" => set!(ts_head_skew_ms),
            "backward_regime_ticks" => set!(backward_regime_ticks),
            _ => {}
        }
    }

    saw_any_field.then_some(s)
}

/// Parse every `genlock-fifo audit` line found in `text` (one call per `\n`-separated
/// line), in the order they appear. Non-audit lines are silently skipped.
pub fn parse_audit_lines(text: &str) -> Vec<AuditSample> {
    text.lines().filter_map(parse_audit_line).collect()
}

/// Group samples by source name, preserving each source's first-seen order (both across
/// groups and within a group — the samples stay in their original chronological order).
pub fn group_by_source(samples: &[AuditSample]) -> Vec<(String, Vec<AuditSample>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<AuditSample>> = HashMap::new();
    for sample in samples {
        if !groups.contains_key(&sample.source) {
            order.push(sample.source.clone());
        }
        groups
            .entry(sample.source.clone())
            .or_default()
            .push(sample.clone());
    }
    order
        .into_iter()
        .map(|name| {
            let group = groups.remove(&name).unwrap_or_default();
            (name, group)
        })
        .collect()
}

/// The per-run answer for one source's captured window: DELTA loss/backpressure counters
/// (last sample minus first) plus the head-skew jitter distribution across every sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditSummary {
    pub source: String,
    /// Number of audit samples in this window (more samples = a longer captured run;
    /// each sample is one ~5s audit tick).
    pub samples: usize,
    /// The effective latency (ms) this window was captured at (from the last sample).
    pub latency_ms: u32,
    pub delta_underruns: u64,
    pub delta_holds: u64,
    pub delta_overruns: u64,
    pub delta_backward_steps: u64,
    /// #1009 — window delta of the cumulative backward-step re-anchor TICK counter; any
    /// movement here means the configured hold was bypassed during the window.
    pub delta_backward_regime_ticks: u64,
    pub delta_dropped_due: u64,
    pub delta_relocks: u64,
    pub delta_late_holds: u64,
    /// Largest `|ts_head_skew_ms|` observed across the window — the worst-case arrival
    /// jitter this reserve had to absorb.
    pub max_abs_head_skew_ms: i64,
    /// Mean `|ts_head_skew_ms|` across the window — the typical arrival jitter.
    pub mean_abs_head_skew_ms: f64,
    /// #757 — SIGNED mean `ts_head_skew_ms` across the window (never `.abs()`'d, unlike
    /// `mean_abs_head_skew_ms` above). This is the correction signal a pre-record phase
    /// calibrator needs: a genlocked source's TRUE cam→strih transit time is approximately
    /// `latency_ms + mean_head_skew_ms` (positive skew = frame arrived AFTER its scheduled
    /// release, i.e. the applied pin under-estimates true transit; negative = the pin has
    /// slack to spare). The `.abs()`'d field above answers "how much jitter", this one
    /// answers "which direction, on average" — needed to reconstruct an absolute latency the
    /// #286 `compute_phase_sync_offsets` kernel can re-pin from, without a full recording.
    pub mean_head_skew_ms: f64,
    /// The deepest FIFO depth/peak observed across the window.
    pub peak_depth: u64,
}

/// Summarize one source's chronologically-ordered sample window into an [`AuditSummary`].
/// Returns `None` for an empty slice (nothing to summarize).
pub fn summarize(samples: &[AuditSample]) -> Option<AuditSummary> {
    let first = samples.first()?;
    let last = samples.last()?;

    let abs_skews: Vec<i64> = samples.iter().map(|s| s.ts_head_skew_ms.abs()).collect();
    let max_abs_head_skew_ms = abs_skews.iter().copied().max().unwrap_or(0);
    let mean_abs_head_skew_ms = abs_skews.iter().sum::<i64>() as f64 / samples.len() as f64;
    // #757: SIGNED mean, no `.abs()` — see the field doc for why the sign matters.
    let mean_head_skew_ms =
        samples.iter().map(|s| s.ts_head_skew_ms).sum::<i64>() as f64 / samples.len() as f64;
    let peak_depth = samples
        .iter()
        .map(|s| s.peak as u64)
        .chain(samples.iter().map(|s| s.depth))
        .max()
        .unwrap_or(0);

    Some(AuditSummary {
        source: last.source.clone(),
        samples: samples.len(),
        latency_ms: last.latency_ms,
        delta_underruns: last.underruns.saturating_sub(first.underruns),
        delta_holds: last.holds.saturating_sub(first.holds),
        delta_overruns: last.overruns.saturating_sub(first.overruns),
        delta_backward_steps: last.backward_steps.saturating_sub(first.backward_steps),
        delta_backward_regime_ticks: last
            .backward_regime_ticks
            .saturating_sub(first.backward_regime_ticks),
        delta_dropped_due: last.dropped_due.saturating_sub(first.dropped_due),
        delta_relocks: last.relocks.saturating_sub(first.relocks),
        delta_late_holds: last.late_holds.saturating_sub(first.late_holds),
        max_abs_head_skew_ms,
        mean_abs_head_skew_ms,
        mean_head_skew_ms,
        peak_depth,
    })
}

/// #757 — JSON-serialize a slice of [`AuditSummary`] as one JSON object keyed by `source`
/// name, each value carrying the fields a pre-record phase calibrator needs
/// (`latency_ms` + `mean_head_skew_ms` reconstructs the absolute cam→strih transit time —
/// see `mean_head_skew_ms`'s own doc). Used by `genlock-jitter-report --json`. Every summary
/// is included unfiltered/unrenamed — a caller needing only e.g. `"NDI cam<N>"` strih sources
/// filters the result afterward, this function never guesses which subset matters.
pub fn summaries_to_json(summaries: &[AuditSummary]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for s in summaries {
        out.insert(
            s.source.clone(),
            serde_json::json!({
                "samples": s.samples,
                "latency_ms": s.latency_ms,
                "mean_head_skew_ms": s.mean_head_skew_ms,
                "mean_abs_head_skew_ms": s.mean_abs_head_skew_ms,
                "max_abs_head_skew_ms": s.max_abs_head_skew_ms,
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// Convenience: group `samples` by source, then [`summarize`] each group. One
/// [`AuditSummary`] per source found, in first-seen order.
pub fn summarize_all(samples: &[AuditSample]) -> Vec<AuditSummary> {
    group_by_source(samples)
        .into_iter()
        .filter_map(|(_source, group)| summarize(&group))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real captured audit line (field order/format mirrors
    /// `vendor/obs-studio/libobs/obs-source.c` `genlock_audit_log`, incl. an OBS-style
    /// timestamp prefix that the parser must skip past).
    const SAMPLE_LINE_CAM1: &str = "14:00:00.001: genlock-fifo audit 'NDI cam1': \
        received=1000 consumed=999 underruns=0 holds=2 overruns=0 backward_steps=0 \
        dropped_due=0 relocks=0 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 \
        (\u{2248}1 frames @ 30.000fps) src_latency_ms=0 global_latency_ms=3 preload=1 \
        (=33 ms) reserve_ms=3 cap=5 empty_run=0 (re-arm@10) ts_present=123456789012 \
        ts_due=987 ts_head_skew_ms=-2 backward_regime_ticks=4 \
        (#70/#97/#126/#147/#148/#184/#235/#245/#401)";

    #[test]
    fn parses_every_field_from_a_real_audit_line() {
        let s = parse_audit_line(SAMPLE_LINE_CAM1).expect("must parse a real audit line");
        assert_eq!(s.source, "NDI cam1");
        assert_eq!(s.received, 1000);
        assert_eq!(s.consumed, 999);
        assert_eq!(s.underruns, 0);
        assert_eq!(s.holds, 2);
        assert_eq!(s.overruns, 0);
        assert_eq!(s.backward_steps, 0);
        assert_eq!(s.dropped_due, 0);
        assert_eq!(s.relocks, 0);
        assert_eq!(s.late_holds, 0);
        assert!(s.locked);
        assert_eq!(s.depth, 3);
        assert_eq!(s.peak, 5);
        assert_eq!(s.latency_ms, 3);
        assert_eq!(s.src_latency_ms, 0);
        assert_eq!(s.global_latency_ms, 3);
        assert_eq!(s.preload, 1);
        assert_eq!(s.reserve_ms, 3);
        assert_eq!(s.cap, 5);
        assert_eq!(s.empty_run, 0);
        assert_eq!(s.ts_present, 123456789012);
        assert_eq!(s.ts_due, 987);
        assert_eq!(s.ts_head_skew_ms, -2);
        // #1009: the re-anchor tick counter parses from its audit token.
        assert_eq!(s.backward_regime_ticks, 4);
    }

    #[test]
    fn backward_regime_ticks_defaults_to_zero_on_a_pre_1009_line() {
        // A log captured from a pre-#1009 build has no backward_regime_ticks= token — the
        // parser must yield 0, never fail the line.
        let old = SAMPLE_LINE_CAM1.replace("backward_regime_ticks=4 ", "");
        let s = parse_audit_line(&old).expect("pre-#1009 line must still parse");
        assert_eq!(s.backward_regime_ticks, 0);
    }

    #[test]
    fn returns_none_for_a_non_audit_line() {
        assert!(parse_audit_line("14:00:00.002: some unrelated OBS log line").is_none());
        assert!(parse_audit_line("").is_none());
    }

    #[test]
    fn parses_a_positive_head_skew_and_an_unlocked_source() {
        let line = SAMPLE_LINE_CAM1
            .replace("ts_head_skew_ms=-2", "ts_head_skew_ms=7")
            .replace("locked=1", "locked=0");
        let s = parse_audit_line(&line).unwrap();
        assert_eq!(s.ts_head_skew_ms, 7);
        assert!(!s.locked);
    }

    #[test]
    fn parses_a_source_name_containing_a_space() {
        // Real rig source names are e.g. "NDI cam1" — the quote-delimited scan must not
        // be confused by the space when the surrounding text is split on whitespace.
        let s = parse_audit_line(SAMPLE_LINE_CAM1).unwrap();
        assert_eq!(s.source, "NDI cam1");
    }

    #[test]
    fn groups_multiple_sources_preserving_first_seen_order() {
        let cam2_first = SAMPLE_LINE_CAM1
            .replace("NDI cam1", "NDI cam2")
            .replace("underruns=0", "underruns=1");
        let text = format!("{cam2_first}\n{SAMPLE_LINE_CAM1}\n{SAMPLE_LINE_CAM1}");
        let samples = parse_audit_lines(&text);
        assert_eq!(samples.len(), 3, "all 3 lines must parse");

        let groups = group_by_source(&samples);
        assert_eq!(groups.len(), 2, "exactly 2 distinct sources");
        assert_eq!(groups[0].0, "NDI cam2", "first-seen source is first");
        assert_eq!(groups[0].1.len(), 1);
        assert_eq!(groups[1].0, "NDI cam1");
        assert_eq!(groups[1].1.len(), 2);
    }

    #[test]
    fn summarize_computes_deltas_between_first_and_last_sample() {
        let mut first = parse_audit_line(SAMPLE_LINE_CAM1).unwrap();
        let mut last = first.clone();
        first.underruns = 5;
        first.holds = 10;
        first.dropped_due = 1;
        first.late_holds = 0;
        first.ts_head_skew_ms = -3;
        last.underruns = 8;
        last.holds = 12;
        last.dropped_due = 4;
        last.late_holds = 2;
        last.ts_head_skew_ms = 6;
        last.peak = 9;
        last.depth = 4;

        let summary = summarize(&[first, last]).expect("2 samples must summarize");
        assert_eq!(summary.source, "NDI cam1");
        assert_eq!(summary.samples, 2);
        assert_eq!(summary.latency_ms, 3);
        assert_eq!(summary.delta_underruns, 3);
        assert_eq!(summary.delta_holds, 2);
        assert_eq!(summary.delta_dropped_due, 3);
        assert_eq!(summary.delta_late_holds, 2);
        assert_eq!(summary.max_abs_head_skew_ms, 6);
        assert!(
            (summary.mean_abs_head_skew_ms - 4.5).abs() < 1e-9,
            "mean(|-3|, |6|) == 4.5, got {}",
            summary.mean_abs_head_skew_ms
        );
        assert_eq!(
            summary.peak_depth, 9,
            "peak_depth is the max across peak/depth"
        );
    }

    #[test]
    fn summarize_returns_none_for_an_empty_window() {
        assert!(summarize(&[]).is_none());
    }

    #[test]
    fn summarize_computes_the_signed_mean_head_skew_not_absolute_757() {
        // #757: a pre-record phase calibrator needs the SIGNED mean skew (direction, not just
        // magnitude) to reconstruct an absolute cam->strih transit time from the applied pin.
        // mean(-3, 6) must be +1.5, never |{-3,6}| mean = 4.5 (that's mean_abs_head_skew_ms).
        let mut first = parse_audit_line(SAMPLE_LINE_CAM1).unwrap();
        let mut last = first.clone();
        first.ts_head_skew_ms = -3;
        last.ts_head_skew_ms = 6;

        let summary = summarize(&[first, last]).expect("2 samples must summarize");
        assert!(
            (summary.mean_head_skew_ms - 1.5).abs() < 1e-9,
            "signed mean(-3, 6) == 1.5, got {}",
            summary.mean_head_skew_ms
        );
        // Sanity: a symmetric window must average toward zero on the SIGNED mean even though
        // the ABS mean stays clearly positive -- proves the two fields really are distinct.
        let mut neg = parse_audit_line(SAMPLE_LINE_CAM1).unwrap();
        let mut pos = neg.clone();
        neg.ts_head_skew_ms = -10;
        pos.ts_head_skew_ms = 10;
        let symmetric = summarize(&[neg, pos]).unwrap();
        assert!(
            (symmetric.mean_head_skew_ms - 0.0).abs() < 1e-9,
            "signed mean(-10, 10) == 0.0, got {}",
            symmetric.mean_head_skew_ms
        );
        assert!(
            (symmetric.mean_abs_head_skew_ms - 10.0).abs() < 1e-9,
            "abs mean(|-10|, |10|) == 10.0 (unchanged, distinct field), got {}",
            symmetric.mean_abs_head_skew_ms
        );
    }

    #[test]
    fn summarize_single_sample_window_has_zero_deltas() {
        let s = parse_audit_line(SAMPLE_LINE_CAM1).unwrap();
        let summary = summarize(std::slice::from_ref(&s)).unwrap();
        assert_eq!(summary.samples, 1);
        assert_eq!(summary.delta_underruns, 0);
        assert_eq!(summary.delta_holds, 0);
        assert_eq!(summary.max_abs_head_skew_ms, s.ts_head_skew_ms.abs());
    }

    #[test]
    fn summarize_all_produces_one_summary_per_source_in_first_seen_order() {
        let cam2 = SAMPLE_LINE_CAM1.replace("NDI cam1", "NDI cam2");
        let text = format!("{SAMPLE_LINE_CAM1}\n{cam2}\n{SAMPLE_LINE_CAM1}");
        let samples = parse_audit_lines(&text);
        let summaries = summarize_all(&samples);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source, "NDI cam1");
        assert_eq!(summaries[0].samples, 2);
        assert_eq!(summaries[1].source, "NDI cam2");
        assert_eq!(summaries[1].samples, 1);
    }

    #[test]
    fn summaries_to_json_carries_source_latency_and_signed_skew_757() {
        let cam2 = SAMPLE_LINE_CAM1
            .replace("NDI cam1", "NDI cam2")
            .replace("ts_head_skew_ms=-2", "ts_head_skew_ms=6");
        let text = format!("{SAMPLE_LINE_CAM1}\n{cam2}");
        let samples = parse_audit_lines(&text);
        let summaries = summarize_all(&samples);
        let json = summaries_to_json(&summaries);
        let obj = json.as_object().expect("must be a JSON object");
        assert_eq!(obj.len(), 2, "one entry per source");

        let cam1 = &obj["NDI cam1"];
        assert_eq!(cam1["latency_ms"], 3);
        assert_eq!(cam1["mean_head_skew_ms"], -2.0);
        assert_eq!(cam1["samples"], 1);

        let cam2 = &obj["NDI cam2"];
        assert_eq!(cam2["latency_ms"], 3);
        assert_eq!(cam2["mean_head_skew_ms"], 6.0);
    }

    #[test]
    fn summaries_to_json_of_empty_input_is_an_empty_object() {
        let json = summaries_to_json(&[]);
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn parse_audit_lines_skips_interleaved_non_audit_noise() {
        let text = format!(
            "some banner\n{SAMPLE_LINE_CAM1}\nanother unrelated line\n{SAMPLE_LINE_CAM1}\n"
        );
        assert_eq!(parse_audit_lines(&text).len(), 2);
    }
}
