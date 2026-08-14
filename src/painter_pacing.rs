//! #859 — PAINTER-PACING ATTRIBUTION for the fused-gate duplicate residual.
//!
//! The fused gate's last red term is a residual of DUPLICATE painted-tick frames in the recorded
//! stream (`all_cambox_continuity` `copies`). The ticket hypothesised ONE shared fault upstream of
//! the splitter, and named the cam2 painter (whose monitor every camera films) as the prime
//! suspect: a missed DRM-vsync deadline paints one logical tick for two refresh cycles, so every
//! camera genuinely captures it twice.
//!
//! ## The discriminator this module makes permanent
//!
//! The painter already logs its own ground truth — `painter-*.csv`, one row per painted frame:
//! `tick,gen_ts_ns,flip_ts_ns` (the logical counter, the generate instant baked into the QR, and
//! the page-flip-COMPLETE = on-screen instant). recording-verdict already parses it (for latency),
//! but NOTHING computes its PACING. So "was the painter itself clean, or did it stall?" had to be
//! hand-mined from the CSV every time — and re-derived from scratch by the next investigator.
//!
//! This module turns that one-off analysis into a tested instrument. From the painter's own
//! `(tick, flip_ts_ns)` sequence it derives:
//!
//! - **painted-tick faults** (a tick repeated / skipped / gone backward) — the painter emitting a
//!   bad LOGICAL sequence; and
//! - **missed DRM-vsync deadlines** — an inter-flip interval `>= 1.5x` the run's own median
//!   (nominal) interval, i.e. the painter held a frame for more than one refresh; plus a stricter
//!   `>= 2x` "duplicate-class stall" that is long enough to strand a captured duplicate at the
//!   30fps sampling rate.
//!
//! A run with ZERO painted faults and ZERO missed deadlines EXONERATES the painter: a residual
//! captured duplicate is then attributable DOWNSTREAM of the page-flip — the monitor panel refresh
//! vs 30fps capture optical beat, or the strih/stream genlock FIFO limit cycle — never the painter.
//! (Live evidence at filing: across 6 real runs incl. the worst 187-copy transient, every run
//! measured 0 duplicates / 0 skips / 0 missed deadlines, max inter-flip ~20ms at a 16.67ms/60fps
//! nominal — the painter was metronomic even while the recorded output carried 187 copies.)
//!
//! This is a REPORT-ONLY attribution instrument surfaced under `all_cambox_continuity.painter_pacing`
//! — it NEVER gates and it changes NO threshold. It mirrors the `painted_tick_gaps.rs` /
//! `residual_events.rs` / `window_gate.rs` crate-root pure-seam pattern so the whole thing is
//! RED->GREEN-verifiable on DEFAULT features, while `src/probe/*` / `recording-verdict.rs` stay
//! CI-only.

use serde::Serialize;

/// One painted frame from the cam2 painter's ground-truth CSV. `flip_ts_ns` is `None` for the older
/// 2-column `tick,gen_ts_ns` log (no page-flip stamp), in which case only the painted-tick sequence
/// (duplicates / skips / non-monotonic) can be assessed, not the flip cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PainterRow {
    pub tick: u32,
    pub flip_ts_ns: Option<i64>,
}

/// Painter-pacing attribution over one painter run. REPORT-ONLY — never gates.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PainterPacing {
    /// painted frames analysed.
    pub rows: usize,
    /// CSV data rows that failed to parse (skipped, not fatal — a report-only instrument must never
    /// newly fail an otherwise-passing verdict).
    pub malformed_rows: usize,
    /// the SAME painted tick appeared twice in a row (a painter software repeat).
    pub painted_duplicates: u32,
    /// a forward jump `> 1` in the painted tick (a painter software skip).
    pub painted_skips: u32,
    /// the painted tick went backward (a painter software fault).
    pub nonmonotonic: u32,
    /// whether at least two rows carried a `flip_ts_ns` (enables the deadline analysis).
    pub have_flip_stamps: bool,
    /// the run's own median inter-flip interval (ns) — the nominal refresh period; `None` if `< 2`
    /// flip stamps.
    pub nominal_flip_interval_ns: Option<i64>,
    /// the largest inter-flip interval observed (ns); `None` if `< 2` flip stamps.
    pub max_flip_interval_ns: Option<i64>,
    /// inter-flip intervals `>= 1.5x` the nominal — the painter missed a DRM-vsync deadline.
    pub missed_deadlines: u32,
    /// inter-flip intervals `>= 2x` the nominal — a stall long enough to strand a CAPTURED duplicate
    /// at the 30fps sampling rate.
    pub duplicate_class_stalls: u32,
}

impl PainterPacing {
    /// The painter emitted a perfect metronomic sequence: no logical-tick fault and no missed
    /// DRM-vsync deadline.
    pub fn is_clean(&self) -> bool {
        self.painted_duplicates == 0
            && self.painted_skips == 0
            && self.nonmonotonic == 0
            && self.missed_deadlines == 0
    }

    /// Attribute the run's residual duplicate count (`all_cambox_continuity` total `copies`) to the
    /// painter or to a downstream stage, combining this pacing with the observed `total_copies`.
    pub fn duplicate_attribution(&self, total_copies: u32) -> String {
        if total_copies == 0 {
            "no-duplicate-residual".to_string()
        } else if self.is_clean() {
            format!(
                "downstream-of-painter (monitor/camera/splitter optical beat or strih/stream \
                 genlock FIFO) -- painter pacing verified CLEAN over {} painted frames \
                 (0 tick duplicates, 0 skips, 0 non-monotonic, 0 missed vsync deadlines)",
                self.rows
            )
        } else {
            format!(
                "painter-pacing-fault -- {} painted duplicates, {} skips, {} non-monotonic, \
                 {} missed vsync deadlines, {} duplicate-class stalls over {} painted frames",
                self.painted_duplicates,
                self.painted_skips,
                self.nonmonotonic,
                self.missed_deadlines,
                self.duplicate_class_stalls,
                self.rows,
            )
        }
    }
}

/// Compute painter pacing over an ordered slice of painted frames.
pub fn analyze(rows: &[PainterRow]) -> PainterPacing {
    // [red] not yet implemented — no pacing computed.
    PainterPacing {
        rows: rows.len(),
        malformed_rows: 0,
        painted_duplicates: 0,
        painted_skips: 0,
        nonmonotonic: 0,
        have_flip_stamps: false,
        nominal_flip_interval_ns: None,
        max_flip_interval_ns: None,
        missed_deadlines: 0,
        duplicate_class_stalls: 0,
    }
}

/// Parse a painter CSV (`tick,gen_ts_ns,flip_ts_ns` — or the older 2-column `tick,gen_ts_ns`) into
/// ordered rows and analyse it. TOLERANT: a data row whose tick (or, when a flip column is present,
/// whose flip stamp) does not parse is COUNTED in `malformed_rows` and skipped, never fatal — a
/// report-only instrument must not turn a passing verdict red over a stray CSV line.
pub fn analyze_csv(text: &str) -> PainterPacing {
    let header = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let has_flip = header.starts_with("tick,gen_ts_ns,flip_ts_ns");

    let mut rows: Vec<PainterRow> = Vec::new();
    let mut malformed = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("tick,") {
            continue; // blank or header
        }
        let cols: Vec<&str> = line.split(',').collect();
        let tick = match cols.first().and_then(|c| c.trim().parse::<u32>().ok()) {
            Some(t) => t,
            None => {
                malformed += 1;
                continue;
            }
        };
        let flip_ts_ns = if has_flip {
            match cols.get(2).and_then(|c| c.trim().parse::<i64>().ok()) {
                Some(f) => Some(f),
                None => {
                    malformed += 1;
                    continue;
                }
            }
        } else {
            None
        };
        rows.push(PainterRow { tick, flip_ts_ns });
    }

    let mut pacing = analyze(&rows);
    pacing.malformed_rows = malformed;
    pacing
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOMINAL: i64 = 16_670_000; // 60fps refresh period in ns

    /// Build a clean N-frame 60fps painter run: monotonic ticks, uniform flip cadence + `jitter_ns`
    /// added to the interval at a few indices to model real-rig jitter.
    fn clean_run(n: u32) -> Vec<PainterRow> {
        let mut rows = Vec::with_capacity(n as usize);
        let mut flip = 1_786_702_719_000_000_000i64;
        for tick in 0..n {
            rows.push(PainterRow {
                tick,
                flip_ts_ns: Some(flip),
            });
            flip += NOMINAL;
        }
        rows
    }

    #[test]
    fn clean_60fps_run_exonerates_the_painter() {
        let p = analyze(&clean_run(600));
        assert_eq!(p.rows, 600);
        assert_eq!(p.painted_duplicates, 0);
        assert_eq!(p.painted_skips, 0);
        assert_eq!(p.nonmonotonic, 0);
        assert!(p.have_flip_stamps);
        assert_eq!(p.nominal_flip_interval_ns, Some(NOMINAL));
        assert_eq!(p.missed_deadlines, 0);
        assert_eq!(p.duplicate_class_stalls, 0);
        assert!(p.is_clean());
        assert_eq!(p.duplicate_attribution(0), "no-duplicate-residual");
        // A residual duplicate against a CLEAN painter is attributed downstream.
        let attr = p.duplicate_attribution(5);
        assert!(attr.contains("downstream-of-painter"), "attr={attr}");
        assert!(attr.contains("verified CLEAN"), "attr={attr}");
    }

    #[test]
    fn real_rig_jitter_up_to_20ms_is_not_a_missed_deadline() {
        // Real 1399731812 finding: intervals 14..20ms at a 16.67ms nominal, none flagged.
        let mut rows = clean_run(200);
        // bump a handful of intervals to 20ms (< 1.5x nominal = 25ms) — must NOT flag.
        for i in [50usize, 90, 130] {
            for r in rows.iter_mut().skip(i) {
                if let Some(f) = r.flip_ts_ns.as_mut() {
                    *f += 3_330_000; // shift the tail forward by 3.33ms => that one interval ~20ms
                }
                break;
            }
        }
        let p = analyze(&rows);
        assert_eq!(p.missed_deadlines, 0, "20ms < 1.5x nominal must not flag");
        assert!(p.max_flip_interval_ns.unwrap() < NOMINAL * 3 / 2);
        assert!(p.is_clean());
    }

    #[test]
    fn a_missed_vsync_deadline_incriminates_the_painter() {
        let mut rows = clean_run(200);
        // stretch ONE interval to 40ms (>= 2x nominal): shift every row from index 100 on by 40ms
        // minus one nominal => the single interval at 99->100 becomes ~40ms, rest stay nominal.
        for r in rows.iter_mut().skip(100) {
            if let Some(f) = r.flip_ts_ns.as_mut() {
                *f += 40_000_000 - NOMINAL;
            }
        }
        let p = analyze(&rows);
        assert_eq!(
            p.missed_deadlines, 1,
            "one ~40ms interval is a missed deadline"
        );
        assert_eq!(p.duplicate_class_stalls, 1, "40ms >= 2x nominal");
        assert!(p.max_flip_interval_ns.unwrap() >= NOMINAL * 2);
        assert!(!p.is_clean());
        let attr = p.duplicate_attribution(3);
        assert!(attr.contains("painter-pacing-fault"), "attr={attr}");
        assert!(attr.contains("1 missed vsync deadlines"), "attr={attr}");
    }

    #[test]
    fn a_repeated_painted_tick_is_a_painter_duplicate() {
        let mut rows = clean_run(10);
        rows[5].tick = rows[4].tick; // repeat tick 4 at index 5
        let p = analyze(&rows);
        assert_eq!(p.painted_duplicates, 1);
        assert!(!p.is_clean());
    }

    #[test]
    fn a_skipped_painted_tick_is_a_painter_skip() {
        let mut rows = clean_run(10);
        for r in rows.iter_mut().skip(6) {
            r.tick += 1; // open a gap: ...5,7,8,9... (skip at 5->7)
        }
        let p = analyze(&rows);
        assert_eq!(p.painted_skips, 1);
        assert_eq!(p.painted_duplicates, 0);
        assert!(!p.is_clean());
    }

    #[test]
    fn a_backward_painted_tick_is_non_monotonic() {
        let mut rows = clean_run(10);
        rows[5].tick = 2; // ...4,2,6... one backward step
        let p = analyze(&rows);
        assert_eq!(p.nonmonotonic, 1);
        assert!(!p.is_clean());
    }

    #[test]
    fn analyze_csv_parses_the_three_column_paint_log() {
        let csv = "tick,gen_ts_ns,flip_ts_ns\n\
                   0,1000,1000000\n\
                   1,1016,17670000\n\
                   2,1033,34340000\n";
        let p = analyze_csv(csv);
        assert_eq!(p.rows, 3);
        assert_eq!(p.malformed_rows, 0);
        assert!(p.have_flip_stamps);
        assert_eq!(p.painted_duplicates, 0);
        assert_eq!(p.nominal_flip_interval_ns, Some(16_670_000));
        assert!(p.is_clean());
    }

    #[test]
    fn analyze_csv_counts_malformed_rows_and_never_bails() {
        let csv = "tick,gen_ts_ns,flip_ts_ns\n\
                   0,1000,1000000\n\
                   NOTANUMBER,1016,17670000\n\
                   2,1033,not_a_flip\n\
                   3,1050,50010000\n";
        let p = analyze_csv(csv);
        assert_eq!(p.malformed_rows, 2, "two bad rows skipped, counted");
        assert_eq!(p.rows, 2, "two good rows retained");
    }

    #[test]
    fn two_column_log_has_no_flip_analysis_but_still_checks_tick_sequence() {
        let csv = "tick,gen_ts_ns\n0,1000\n1,1016\n1,1033\n";
        let p = analyze_csv(csv);
        assert!(!p.have_flip_stamps);
        assert_eq!(p.nominal_flip_interval_ns, None);
        assert_eq!(p.missed_deadlines, 0);
        assert_eq!(p.painted_duplicates, 1, "tick 1 repeated");
    }
}
