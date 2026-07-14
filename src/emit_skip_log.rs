//! camera-box #752 — rate-limit the #707 genlock emit-gate-skip diagnostic.
//!
//! `feat(#707)` (`95b789a53`) added a `#707 genlock emit-gate SKIPPED N boundary interval(s)`
//! WARN emitted once PER gate call that skipped a boundary — ~10×/second on a box that is
//! skipping. Measured live on CAM1 (10.77.9.61, a 3-core box): ~25,548 journal lines / 60s
//! driving rsyslogd ~37% + systemd-journald ~15% CPU — roughly half a core spent purely on
//! logging. On the 3-core cam boxes that is a CPU-STARVATION FEEDBACK LOOP: the emit-gate-skip
//! logs → rsyslog/journald CPU → the NDI emit thread is starved → MORE skips → more logs,
//! aggravating the very #707 emit deficit the diagnostic is trying to observe.
//!
//! This pure accumulator coalesces the per-skip events into ONE aggregated WARN per 5s
//! Streaming-report window (count of skip events + total boundaries skipped), matching the
//! existing 5s report cadence — the diagnostic keeps its signal (a grep on `#707 genlock
//! emit-gate SKIPPED` still finds it, with a count) without the log-volume feedback loop.
//!
//! Pure Tier-0 (default features, no probe): the caller (`src/main.rs`) records a skip event per
//! gate call inside the capture loop, and drains one summary per 5s report. Unit-tested off-rig.

/// Accumulates emit-gate boundary-skip events between flushes. Every capture-loop gate call that
/// skipped >= 1 boundary calls [`EmitGateSkipLog::record`]; the 5s Streaming report calls
/// [`EmitGateSkipLog::take`] to drain a single aggregated summary (event count + total boundaries)
/// and reset. Not `Copy` — it owns the between-flush counters.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EmitGateSkipLog {
    events: u64,
    total_skipped: u64,
}

impl EmitGateSkipLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record ONE gate call that skipped `skipped` boundary intervals. A `skipped` of 0 is ignored
    /// (a normal single-interval advance / a decimated poll is NOT a skip), so a caller that calls
    /// `record` on every poll can never inflate the event count with non-skips.
    pub fn record(&mut self, skipped: u64) {
        if skipped == 0 {
            return;
        }
        self.events = self.events.saturating_add(1);
        self.total_skipped = self.total_skipped.saturating_add(skipped);
    }

    /// True while nothing has been recorded since the last flush (lets the report skip the WARN).
    pub fn is_empty(&self) -> bool {
        self.events == 0
    }

    /// Drain the accumulated `(events, total_skipped)` and RESET, or `None` when nothing was
    /// recorded this window — so the 5s report emits at most ONE aggregated WARN, and none at all
    /// on a clean window (the whole point of the #752 throttle).
    pub fn take(&mut self) -> Option<(u64, u64)> {
        if self.events == 0 {
            return None;
        }
        let out = (self.events, self.total_skipped);
        self.events = 0;
        self.total_skipped = 0;
        Some(out)
    }
}

/// The single aggregated WARN string for a Streaming-report window that saw skips: `events` gate
/// calls skipped `total_skipped` boundary intervals over the last ~`window_secs`s. Keeps the
/// `#707 genlock emit-gate SKIPPED` phrase (existing log greps still match) but as ONE line per
/// window instead of ~10/s.
pub fn skip_summary_warning(events: u64, total_skipped: u64, window_secs: u64) -> String {
    format!(
        "#707 genlock emit-gate SKIPPED boundaries in {events} gate call(s) totalling \
         {total_skipped} boundary interval(s) over the last ~{window_secs}s (rate-limited #752 \
         aggregate — a clock discontinuity or a stalled/starved emit poll leapt past frame(s) that \
         were never emitted; per-skip detail is throttled to stop the rsyslogd/journald CPU \
         feedback loop, see #752)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_ignores_zero_and_counts_only_real_skips() {
        let mut log = EmitGateSkipLog::new();
        assert!(log.is_empty());
        log.record(0); // a non-skip poll must not count
        assert!(log.is_empty(), "a 0-boundary 'skip' must not register an event");
        log.record(2);
        log.record(1);
        assert!(!log.is_empty());
        let (events, total) = log.take().expect("two real skips must produce a summary");
        assert_eq!(events, 2, "two skip events");
        assert_eq!(total, 3, "2 + 1 boundaries skipped");
    }

    #[test]
    fn take_coalesces_many_skips_into_one_summary_then_resets() {
        let mut log = EmitGateSkipLog::new();
        // Simulate ~10 emit-gate skips in one 5s window (the ~10/s log storm #752 kills).
        let mut expected_total = 0u64;
        for k in 0..10u64 {
            let skipped = 1 + (k % 3); // 1,2,3,1,2,3,...
            log.record(skipped);
            expected_total += skipped;
        }
        let (events, total) = log.take().expect("a window with skips yields exactly one summary");
        assert_eq!(events, 10, "all 10 skip events coalesced into ONE summary");
        assert_eq!(total, expected_total);
        // Drained + reset → the NEXT (clean) window emits nothing.
        assert_eq!(
            log.take(),
            None,
            "take() must reset — a clean following window logs NOTHING"
        );
        assert!(log.is_empty());
    }

    #[test]
    fn take_on_a_clean_window_is_none() {
        let mut log = EmitGateSkipLog::new();
        assert_eq!(
            log.take(),
            None,
            "a window with no skips must produce no WARN at all"
        );
    }

    #[test]
    fn summary_names_events_total_window_and_keeps_the_707_grep_tag() {
        let s = skip_summary_warning(10, 24, 5);
        assert!(s.contains("#707"), "keep the #707 log-grep tag");
        assert!(s.contains("genlock emit-gate SKIPPED"), "keep the existing phrase for greps");
        assert!(s.contains("10"), "name the event count");
        assert!(s.contains("24"), "name the total boundaries skipped");
        assert!(s.contains('5'), "name the window seconds");
        assert!(s.contains("#752"), "mark it as the rate-limited aggregate");
    }
}
