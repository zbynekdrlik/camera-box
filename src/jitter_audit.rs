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
                // #1009: the gate-facing hold-bypass signal (0 on a healthy window).
                "delta_backward_regime_ticks": s.delta_backward_regime_ticks,
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

// ============================================================================
// #874 — send-side (NDI OUTPUT / FILTER) audit parser.
//
// The genlock build now emits, every ~5s per NDI sender, a send-path audit line
// mirroring the input-side `genlock-fifo audit` above:
//
//   genlock-ndi-output audit '<name>': offered=N sent=N dropped=N \
//       send_wait_ms=X.XXX max_send_wait_ms=X.XXX (#874)
//   genlock-ndi-filter audit '<ndi name>': offered=N sent=N \
//       send_wait_ms=X.XXX max_send_wait_ms=X.XXX mutex_wait_ms=X.XXX \
//       max_mutex_wait_ms=X.XXX (#874)
//
// (`vendor/distroav/src/ndi-output.cpp` `ndi_output_rawvideo` and
// `vendor/distroav/src/ndi-filter.cpp` `ndi_filter_raw_video`). This is the pure
// Tier-0 parser that turns those lines into the WINDOW-DELTA answer the issue-707
// fork needs: were frames never offered (fault upstream in libobs), offered but
// dropped, or offered and blocked inside the send call (receiver backpressure)?
// It is the symmetric twin of `parse_audit_line`/`summarize` for the send side.

/// Which send path emitted the line: the single main `ndi_output` sender
/// (`genlock-ndi-output`) or one of the aux `ndi_filter` senders
/// (`genlock-ndi-filter`: interkom / MULTIVIEW / Grading).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendAuditKind {
    Output,
    Filter,
}

/// One parsed send-path audit sample. Mirrors [`AuditSample`] for the send side.
/// `offered`/`sent` are CUMULATIVE frame counters since the sender was created;
/// `send_wait_ms` is the CUMULATIVE ms spent inside the send call (lifetime
/// total), `max_send_wait_ms` the lifetime PEAK single-call wait. The mutex pair
/// is present only on the filter line (the aux senders time acquiring the shared
/// send mutex separately from the send itself) — `None` on an output line.
#[derive(Debug, Clone, PartialEq)]
pub struct SendAuditSample {
    pub kind: SendAuditKind,
    /// The NDI sender name (the quoted `'%s'` in the log line), e.g. `"2ME PGM"`.
    pub name: String,
    pub offered: u64,
    pub sent: u64,
    pub send_wait_ms: f64,
    pub max_send_wait_ms: f64,
    pub mutex_wait_ms: Option<f64>,
    pub max_mutex_wait_ms: Option<f64>,
}

impl SendAuditSample {
    /// Frames offered but not sent = `offered - sent` (saturating). The output
    /// line emits this verbatim as `dropped=`; the filter line does not — it is
    /// derived identically either way, so it is computed rather than stored (the
    /// emitted `dropped=` token is skipped like any other unrecognized token).
    /// The derivation is intentionally authoritative — the C++ emits `dropped=`
    /// the same way — and the parser test asserts the emitted `dropped=6000`
    /// equals this derived value, so the two can never silently diverge.
    pub fn dropped(&self) -> u64 {
        self.offered.saturating_sub(self.sent)
    }
}

/// Parse ONE send-path audit line (`genlock-ndi-output` / `genlock-ndi-filter`)
/// into a [`SendAuditSample`]. Returns `None` if the line carries neither send
/// marker (so input `genlock-fifo audit` lines and any other log noise are safely
/// ignored — the input and send parsers can run over the SAME log independently).
/// Whitespace-token `key=value` scan, exactly like [`parse_audit_line`].
pub fn parse_send_audit_line(line: &str) -> Option<SendAuditSample> {
    const OUT_MARK: &str = "genlock-ndi-output audit '";
    const FIL_MARK: &str = "genlock-ndi-filter audit '";
    let (kind, mark_at, mark_len) = line
        .find(OUT_MARK)
        .map(|i| (SendAuditKind::Output, i, OUT_MARK.len()))
        .or_else(|| {
            line.find(FIL_MARK)
                .map(|i| (SendAuditKind::Filter, i, FIL_MARK.len()))
        })?;
    let after_mark = &line[mark_at + mark_len..];
    let quote_end = after_mark.find('\'')?;
    let name = after_mark[..quote_end].to_string();

    let mut s = SendAuditSample {
        kind,
        name,
        offered: 0,
        sent: 0,
        send_wait_ms: 0.0,
        max_send_wait_ms: 0.0,
        mutex_wait_ms: None,
        max_mutex_wait_ms: None,
    };
    let mut saw_any_field = false;

    // Same whitespace-token `key=value` scan as `parse_audit_line`: unrecognized
    // tokens (the `'%s':` name-quote fragment, the derived `dropped=` on the output
    // line, the `(#874)` decoration) are simply skipped.
    for tok in line[mark_at..].split_whitespace() {
        let Some(eq) = tok.find('=') else {
            continue;
        };
        let key = &tok[..eq];
        let val = &tok[eq + 1..];
        match key {
            "offered" => {
                if let Ok(v) = val.parse() {
                    s.offered = v;
                    saw_any_field = true;
                }
            }
            "sent" => {
                if let Ok(v) = val.parse() {
                    s.sent = v;
                    saw_any_field = true;
                }
            }
            "send_wait_ms" => {
                if let Ok(v) = val.parse() {
                    s.send_wait_ms = v;
                    saw_any_field = true;
                }
            }
            "max_send_wait_ms" => {
                if let Ok(v) = val.parse() {
                    s.max_send_wait_ms = v;
                    saw_any_field = true;
                }
            }
            "mutex_wait_ms" => {
                if let Ok(v) = val.parse() {
                    s.mutex_wait_ms = Some(v);
                    saw_any_field = true;
                }
            }
            "max_mutex_wait_ms" => {
                if let Ok(v) = val.parse() {
                    s.max_mutex_wait_ms = Some(v);
                    saw_any_field = true;
                }
            }
            _ => {}
        }
    }

    saw_any_field.then_some(s)
}

/// Parse every send-path audit line found in `text`, in order. Non-send lines
/// (input audit lines, noise) are silently skipped.
pub fn parse_send_audit_lines(text: &str) -> Vec<SendAuditSample> {
    text.lines().filter_map(parse_send_audit_line).collect()
}

/// The per-run answer for one sender's captured window. The load-bearing outputs
/// are the WINDOW DELTAS (last-minus-first of the cumulative counters):
/// `delta_offered`/`delta_sent`/`delta_dropped` and `delta_send_wait_ms` — the
/// issue-707 discriminator. `max_send_wait_ms` (and the filter-only mutex pair)
/// are carried through from the last sample.
#[derive(Debug, Clone, PartialEq)]
pub struct SendAuditSummary {
    pub kind: SendAuditKind,
    pub name: String,
    /// Number of audit samples in this window (each is one ~5s tick).
    pub samples: usize,
    pub delta_offered: u64,
    pub delta_sent: u64,
    /// Window-scoped honest drop = `delta_offered - delta_sent` (saturating) —
    /// the #707 discriminator numerator: frames offered DURING the window that
    /// were never sent DURING the window.
    pub delta_dropped: u64,
    /// Cumulative send-call block time (ms) accrued DURING the window. Large with
    /// `delta_dropped > 0` => the send is blocking (receiver/transport
    /// backpressure); near-zero with `delta_dropped > 0` => frames never reached
    /// the send call (fault upstream in libobs's output path).
    pub delta_send_wait_ms: f64,
    /// Lifetime peak single-call send wait (ms) at window end (last sample).
    pub max_send_wait_ms: f64,
    /// Filter-only: window delta of the cumulative mutex-acquire wait (ms). `None`
    /// for an output summary. A high value with a low `delta_send_wait_ms` means
    /// this sender is queued behind another lock holder, not slow on its own send.
    pub delta_mutex_wait_ms: Option<f64>,
    /// Filter-only: lifetime peak mutex-acquire wait (ms) at window end.
    pub max_mutex_wait_ms: Option<f64>,
}

/// Summarize one sender's chronologically-ordered window into a
/// [`SendAuditSummary`]. Returns `None` for an empty slice.
pub fn summarize_send(samples: &[SendAuditSample]) -> Option<SendAuditSummary> {
    let first = samples.first()?;
    let last = samples.last()?;

    let delta_offered = last.offered.saturating_sub(first.offered);
    let delta_sent = last.sent.saturating_sub(first.sent);
    // Cumulative timings only ever increase in practice; `.max(0.0)` keeps a
    // truncated/rewound log from producing a negative in-window figure.
    let delta_send_wait_ms = (last.send_wait_ms - first.send_wait_ms).max(0.0);
    // Filter-only: `None` on an output window; on a filter window the delta of the
    // cumulative mutex-acquire wait (first sample may predate the field only on a
    // mixed capture, so treat a missing first as 0).
    let delta_mutex_wait_ms = last
        .mutex_wait_ms
        .map(|l| (l - first.mutex_wait_ms.unwrap_or(0.0)).max(0.0));

    Some(SendAuditSummary {
        kind: last.kind,
        name: last.name.clone(),
        samples: samples.len(),
        delta_offered,
        delta_sent,
        delta_dropped: delta_offered.saturating_sub(delta_sent),
        delta_send_wait_ms,
        max_send_wait_ms: last.max_send_wait_ms,
        delta_mutex_wait_ms,
        max_mutex_wait_ms: last.max_mutex_wait_ms,
    })
}

/// Group `samples` by `(kind, name)`, then [`summarize_send`] each group. One
/// summary per distinct sender, in first-seen order.
pub fn summarize_send_all(samples: &[SendAuditSample]) -> Vec<SendAuditSummary> {
    let mut order: Vec<(SendAuditKind, String)> = Vec::new();
    let mut groups: HashMap<(SendAuditKind, String), Vec<SendAuditSample>> = HashMap::new();
    for sample in samples {
        let key = (sample.kind, sample.name.clone());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(sample.clone());
    }
    order
        .into_iter()
        .filter_map(|key| {
            let group = groups.remove(&key).unwrap_or_default();
            summarize_send(&group)
        })
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
    fn summarize_deltas_the_backward_regime_tick_counter_1009() {
        // The gate-facing signal is the WINDOW DELTA of the cumulative counter — any
        // movement means the configured hold was bypassed during the window.
        let mut first = parse_audit_line(SAMPLE_LINE_CAM1).unwrap();
        let mut last = first.clone();
        first.backward_regime_ticks = 4;
        last.backward_regime_ticks = 9;
        let s = summarize(&[first, last]).unwrap();
        assert_eq!(s.delta_backward_regime_ticks, 5);
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

    // ===================== #874 send-side audit parser =====================

    // Real captured-style send-path lines (exact format from
    // vendor/distroav/src/ndi-output.cpp / ndi-filter.cpp, incl. an OBS timestamp
    // prefix the parser must skip and the trailing `(#874)` decoration).
    const OUT_LINE: &str = "03:00:00.001: genlock-ndi-output audit '2ME PGM': \
        offered=9000 sent=3000 dropped=6000 send_wait_ms=120000.500 \
        max_send_wait_ms=33.200 (#874)";
    const FIL_LINE: &str = "03:00:00.002: genlock-ndi-filter audit 'interkom': \
        offered=9000 sent=8990 send_wait_ms=45.100 max_send_wait_ms=12.300 \
        mutex_wait_ms=5.400 max_mutex_wait_ms=1.200 (#874)";

    #[test]
    fn parses_an_output_send_audit_line_874() {
        let s = parse_send_audit_line(OUT_LINE).expect("must parse an output audit line");
        assert_eq!(s.kind, SendAuditKind::Output);
        assert_eq!(s.name, "2ME PGM");
        assert_eq!(s.offered, 9000);
        assert_eq!(s.sent, 3000);
        assert!((s.send_wait_ms - 120000.500).abs() < 1e-6);
        assert!((s.max_send_wait_ms - 33.200).abs() < 1e-6);
        assert_eq!(s.mutex_wait_ms, None, "output line has no mutex fields");
        assert_eq!(s.max_mutex_wait_ms, None);
        assert_eq!(s.dropped(), 6000, "dropped = offered - sent");
    }

    #[test]
    fn parses_a_filter_send_audit_line_874() {
        let s = parse_send_audit_line(FIL_LINE).expect("must parse a filter audit line");
        assert_eq!(s.kind, SendAuditKind::Filter);
        assert_eq!(s.name, "interkom");
        assert_eq!(s.offered, 9000);
        assert_eq!(s.sent, 8990);
        assert!((s.send_wait_ms - 45.100).abs() < 1e-6);
        assert!((s.max_send_wait_ms - 12.300).abs() < 1e-6);
        assert!((s.mutex_wait_ms.expect("filter has mutex_wait_ms") - 5.400).abs() < 1e-6);
        assert!((s.max_mutex_wait_ms.expect("filter has max_mutex_wait_ms") - 1.200).abs() < 1e-6);
        assert_eq!(s.dropped(), 10);
    }

    #[test]
    fn a_send_name_with_a_space_parses_874() {
        // "2ME PGM" carries a space — the quote-delimited scan must not be confused
        // by it when the surrounding text is split on whitespace.
        let s = parse_send_audit_line(OUT_LINE).unwrap();
        assert_eq!(s.name, "2ME PGM");
    }

    #[test]
    fn send_parser_rejects_the_input_audit_line_and_noise_874() {
        // The INPUT-side genlock-fifo line must NOT parse as a send line, and vice
        // versa — the two parsers coexist over the same log.
        assert!(parse_send_audit_line(SAMPLE_LINE_CAM1).is_none());
        assert!(parse_send_audit_line("03:00:00: some unrelated OBS log line").is_none());
        assert!(parse_send_audit_line("").is_none());
        // And the input parser must ignore the send lines.
        assert!(parse_audit_line(OUT_LINE).is_none());
        assert!(parse_audit_line(FIL_LINE).is_none());
    }

    #[test]
    fn parse_send_audit_lines_picks_only_send_lines_from_a_mixed_log_874() {
        let text = format!("banner\n{SAMPLE_LINE_CAM1}\n{OUT_LINE}\nnoise\n{FIL_LINE}\n");
        let send = parse_send_audit_lines(&text);
        assert_eq!(send.len(), 2, "only the 2 send lines");
        assert_eq!(send[0].kind, SendAuditKind::Output);
        assert_eq!(send[1].kind, SendAuditKind::Filter);
        // The input parser over the SAME text still gets only the input line.
        assert_eq!(parse_audit_lines(&text).len(), 1);
    }

    #[test]
    fn summarize_send_computes_window_deltas_874() {
        let first = parse_send_audit_line(OUT_LINE).unwrap();
        let mut last = first.clone();
        last.offered = 18000;
        last.sent = 6000;
        last.send_wait_ms = 240000.500; // +120000.000 accrued in-window
        last.max_send_wait_ms = 40.000;
        let s = summarize_send(&[first, last]).expect("2 samples summarize");
        assert_eq!(s.kind, SendAuditKind::Output);
        assert_eq!(s.name, "2ME PGM");
        assert_eq!(s.samples, 2);
        assert_eq!(s.delta_offered, 9000);
        assert_eq!(s.delta_sent, 3000);
        assert_eq!(s.delta_dropped, 6000, "9000 offered - 3000 sent in-window");
        assert!((s.delta_send_wait_ms - 120000.000).abs() < 1e-3);
        assert!((s.max_send_wait_ms - 40.000).abs() < 1e-6);
        assert_eq!(s.delta_mutex_wait_ms, None);
    }

    #[test]
    fn summarize_send_distinguishes_blocking_from_upstream_707() {
        // The whole point: same drop, two causes, separated by delta_send_wait_ms.
        // (a) BLOCKING send — dropped>0 AND a large in-window block time.
        let mut a0 = parse_send_audit_line(OUT_LINE).unwrap();
        a0.offered = 0;
        a0.sent = 0;
        a0.send_wait_ms = 0.0;
        let mut a1 = a0.clone();
        a1.offered = 9000;
        a1.sent = 3000;
        a1.send_wait_ms = 100000.0;
        let blocking = summarize_send(&[a0, a1]).unwrap();
        assert_eq!(blocking.delta_dropped, 6000);
        assert!(
            blocking.delta_send_wait_ms > 1000.0,
            "large block time => backpressure"
        );

        // (b) UPSTREAM — same drop, but the send call barely blocked.
        let mut b0 = parse_send_audit_line(OUT_LINE).unwrap();
        b0.offered = 0;
        b0.sent = 0;
        b0.send_wait_ms = 0.0;
        let mut b1 = b0.clone();
        b1.offered = 9000;
        b1.sent = 3000;
        b1.send_wait_ms = 2.5;
        let upstream = summarize_send(&[b0, b1]).unwrap();
        assert_eq!(upstream.delta_dropped, 6000);
        assert!(
            upstream.delta_send_wait_ms < 10.0,
            "near-zero block time => fault upstream"
        );
    }

    #[test]
    fn summarize_send_carries_filter_mutex_deltas_874() {
        let first = parse_send_audit_line(FIL_LINE).unwrap();
        let mut last = first.clone();
        last.mutex_wait_ms = Some(9.400); // +4.000 in-window
        last.max_mutex_wait_ms = Some(2.000);
        let s = summarize_send(&[first, last]).unwrap();
        assert!((s.delta_mutex_wait_ms.expect("filter carries mutex delta") - 4.000).abs() < 1e-6);
        assert!((s.max_mutex_wait_ms.expect("filter carries max mutex") - 2.000).abs() < 1e-6);
    }

    #[test]
    fn summarize_send_all_groups_by_kind_and_name_first_seen_874() {
        let fil2 = FIL_LINE.replace("interkom", "MULTIVIEW");
        let text = format!("{OUT_LINE}\n{FIL_LINE}\n{fil2}\n{OUT_LINE}\n{FIL_LINE}");
        let send = parse_send_audit_lines(&text);
        let sums = summarize_send_all(&send);
        assert_eq!(
            sums.len(),
            3,
            "2ME PGM output + interkom + MULTIVIEW filters"
        );
        assert_eq!(
            (sums[0].kind, sums[0].name.as_str()),
            (SendAuditKind::Output, "2ME PGM")
        );
        assert_eq!(sums[0].samples, 2);
        assert_eq!(
            (sums[1].kind, sums[1].name.as_str()),
            (SendAuditKind::Filter, "interkom")
        );
        assert_eq!(sums[1].samples, 2);
        assert_eq!(
            (sums[2].kind, sums[2].name.as_str()),
            (SendAuditKind::Filter, "MULTIVIEW")
        );
        assert_eq!(sums[2].samples, 1);
    }

    #[test]
    fn summarize_send_empty_window_is_none_and_single_sample_zero_deltas_874() {
        assert!(summarize_send(&[]).is_none());
        let s = parse_send_audit_line(OUT_LINE).unwrap();
        let sum = summarize_send(std::slice::from_ref(&s)).unwrap();
        assert_eq!(sum.samples, 1);
        assert_eq!(sum.delta_offered, 0);
        assert_eq!(sum.delta_sent, 0);
        assert_eq!(sum.delta_dropped, 0);
        assert!((sum.delta_send_wait_ms - 0.0).abs() < 1e-9);
    }

    #[test]
    fn send_delta_dropped_never_underflows_874() {
        // Defensive: a counter that appears to go backward (impossible in practice,
        // but a truncated/rewound log must not panic) saturates to 0.
        let mut first = parse_send_audit_line(OUT_LINE).unwrap();
        first.offered = 100;
        first.sent = 100;
        let mut last = first.clone();
        last.offered = 90;
        last.sent = 95; // both "decreased"
        let s = summarize_send(&[first, last]).unwrap();
        assert_eq!(s.delta_offered, 0);
        assert_eq!(s.delta_sent, 0);
        assert_eq!(s.delta_dropped, 0);
    }
}
