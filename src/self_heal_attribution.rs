//! #895 — a `capture_rate_selfheal` (#663) USB reset firing during an E2E measurement window must
//! never be reported as `frozen_leg` (a camera fault). Pure Tier-0 module (no probe deps), extends
//! `frozen_leg`'s classification rather than duplicating it — unit-tests via
//! `cargo test --lib self_heal_attribution # airuleset:build-ok`, mirroring `frozen_leg`'s own
//! crate-root pattern.
//!
//! ## Why the existing mid-recording capture-rate recheck does not already cover this
//!
//! `scripts/lib/capture-rate-guard.sh`'s mid-recording check (wired at `recording-e2e.sh`'s
//! `[7b/8]`, a separate, already-shipped ticket) greps ONLY the literal
//! `"#656 capture-delivery-rate DEFECTIVE"` text — the WARN `src/main.rs` logs on its
//! `jitter_confirmed` branch. But the self-heal trigger has a SECOND, independent branch,
//! `sustained_confirmed` (the narrower, longer SUSTAINED-band tolerance for a chronic deviation
//! that stays inside the wide jitter envelope), which logs a DIFFERENT literal:
//! `"#717 capture-delivery-rate SUSTAINED defect: ..."`. A self-heal reset triggered via the
//! sustained branch alone is therefore invisible to that existing grep, sails through unflagged,
//! and its resulting duplicate/stale frames get classified `frozen_leg` on the camera.
//!
//! ## The fix
//!
//! Key on the RESET event itself (`"#663 self-heal: USB reset attempt #N succeeded"`,
//! `scripts/lib/self-heal-attribution.sh`) rather than either upstream detection band's WARN text
//! — this is the ONE signal both trigger branches share (see `src/main.rs`'s shared
//! `SelfHealDecision::Heal` match arm), so a hypothetical future THIRD detection band is
//! automatically covered too. This module takes the SAME per-window primitives
//! `frozen_leg::frozen_leg_report` classifies, plus the harness's captured reset events, and
//! re-attributes any window that would classify `Frozen` but correlates (same camera, timestamp
//! inside/near the window) with a reset event to a NEW, distinct `self_heal_reset` bucket instead.
//!
//! ## Never silently tolerate the underlying rate defect (#895 acceptance criterion 4)
//!
//! ALLOW is the decision, not SUPPRESS: self-heal keeps firing during a measurement (the
//! underlying over-rate defect is real and tracked separately — disabling the safety net would
//! trade today's misdiagnosis for a worse, silent one). But a reset firing at all during a
//! measurement is itself a real run-integrity concern — [`SelfHealAttributionReport::any_self_heal`]
//! stays true even for a reset event that never correlates to any classified window, so the run
//! still fails, honestly, for the right reason.

use crate::frozen_leg::{classify_leg, FrozenLeg, LegHealth, SegmentLeg, StaleReplayLeg};

/// The KIND of run-integrity restart event correlated against a recording window (issue 946 /
/// issue 910). All three explain a window's staleness as a run-integrity event, NOT a camera
/// fault, and correlate through the SAME engine — only the label/message/JSON `kind` differs.
///
/// - [`SelfHealReset`](RestartEventKind::SelfHealReset): `capture_rate_selfheal` (#663) USB-reset
///   the capture device (the original #895 event).
/// - [`CaptureWedge`](RestartEventKind::CaptureWedge): the out-of-band capture-wedge watchdog
///   (issue 945, `CAPTURE_WEDGE_EXIT_CODE` 79) forced a restart because the blocking V4L2 dequeue
///   never returned; its CRITICAL line lands in the box's journal / burn-instance log.
/// - [`EmitFreeze`](RestartEventKind::EmitFreeze): the out-of-band emit-freeze watchdog (issue 944,
///   `EMIT_FREEZE_EXIT_CODE` 81) forced a restart because no good frame reached NDI while the
///   capture thread stayed alive (a silent-frozen output).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartEventKind {
    /// `capture_rate_selfheal` (#663) USB reset — the original #895 event, the default kind for a
    /// legacy untagged `cambox:epoch_ns` token.
    #[default]
    SelfHealReset,
    /// issue-945 capture-wedge watchdog restart (exit code 79).
    CaptureWedge,
    /// issue-944 emit-freeze watchdog restart (exit code 81).
    EmitFreeze,
}

impl RestartEventKind {
    /// The stable, machine-readable label used in the CLI token, the report JSON, and the operator
    /// message prefix — the SAME strings `scripts/lib/self-heal-attribution.sh`'s recognised-event
    /// table keys on, so the two sides can never drift.
    pub fn label(&self) -> &'static str {
        match self {
            RestartEventKind::SelfHealReset => "self_heal_reset",
            RestartEventKind::CaptureWedge => "capture_wedge",
            RestartEventKind::EmitFreeze => "emit_freeze",
        }
    }

    /// Inverse of [`label`](RestartEventKind::label). `None` for any unrecognised label — a
    /// malformed harness token is dropped, never panics.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "self_heal_reset" => Some(RestartEventKind::SelfHealReset),
            "capture_wedge" => Some(RestartEventKind::CaptureWedge),
            "emit_freeze" => Some(RestartEventKind::EmitFreeze),
            _ => None,
        }
    }

    /// The distinctly-worded reason phrase for the operator message — every kind ends with
    /// "NOT a frozen camera" so a human/CI reader can never mistake a re-attributed window for a
    /// camera fault (the #895 acceptance-criteria requirement, extended to all restart kinds).
    pub fn detail(&self) -> &'static str {
        match self {
            RestartEventKind::SelfHealReset => {
                "capture_rate_selfheal USB-reset fired inside this window, NOT a frozen camera"
            }
            RestartEventKind::CaptureWedge => {
                "the capture/emit thread WEDGED (issue 945, exit 79) and the process restarted \
                 inside this window, NOT a frozen camera"
            }
            RestartEventKind::EmitFreeze => {
                "the NDI output FROZE (issue 944, exit 81) and the process restarted inside this \
                 window, NOT a frozen camera"
            }
        }
    }
}

/// One run-integrity restart event, correlated against a recording window. Originally #895's
/// `#663` self-heal reset (hence the name and the `--self-heal-reset` flag); as of issue 946 it
/// also carries `kind` so the issue-945 capture-wedge and issue-944 emit-freeze restarts flow
/// through the SAME correlation engine. Surfaced in the box's own journal AND — during an E2E burn
/// — its `/tmp/cbox-burn*.log` (issue 910), captured by the harness right after `StopRecord`
/// (`recording-e2e.sh`). `at_ns` is Unix-epoch nanoseconds — the SAME clock domain
/// `SegmentLeg::start_ns`/`end_ns` already use, so no cross-domain conversion is needed to test
/// for window overlap.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfHealResetEvent {
    pub kind: RestartEventKind,
    pub cambox: String,
    pub at_ns: i64,
}

impl SelfHealResetEvent {
    /// Parse ONE CLI token. Accepts BOTH the new kind-tagged `"<kind>:<cambox>:<epoch_ns>"`
    /// (`recording-verdict.rs`'s `--restart-event`, issue 946) AND the legacy
    /// `"<cambox>:<epoch_ns>"` (`--self-heal-reset`, #895 — defaults to
    /// [`RestartEventKind::SelfHealReset`], keeping every existing wiring intact). Camboxes never
    /// contain `:` and the epoch is numeric, so the field count is an unambiguous discriminator.
    /// Returns `None` on any malformed token (unknown kind label, empty cambox,
    /// non-numeric/unparseable ns) — the caller decides whether to warn/ignore; this never panics
    /// on bad harness input.
    pub fn parse(token: &str) -> Option<Self> {
        let parts: Vec<&str> = token.splitn(3, ':').collect();
        let (kind, cambox, ns_str) = match parts.as_slice() {
            [cambox, ns] => (RestartEventKind::SelfHealReset, *cambox, *ns),
            [k, cambox, ns] => (RestartEventKind::from_label(k)?, *cambox, *ns),
            _ => return None,
        };
        if cambox.is_empty() {
            return None;
        }
        let at_ns: i64 = ns_str.trim().parse().ok()?;
        Some(SelfHealResetEvent {
            kind,
            cambox: cambox.to_string(),
            at_ns,
        })
    }
}

/// Correlation margin either side of a window's own `[start_ns, end_ns)` span within which a
/// same-camera self-heal reset event is considered to have CAUSED that window's staleness. #894's
/// own live evidence showed near-exact coincidence (the reset logged ~3s into the affected segment
/// window), but this margin absorbs journal/window-boundary jitter without over-fitting to that
/// one data point — half of `frozen_leg::FROZEN_SUSTAINED_SECS`, generous relative to the ~30s
/// window granularity these segments already run at.
pub const SELF_HEAL_CORRELATION_MARGIN_NS: i64 = 5_000_000_000;

/// A window that WOULD have hard-failed as `frozen_leg` but instead correlates with a `#663`
/// self-heal reset event on the SAME camera near the SAME window — attributed to the real cause,
/// never the camera.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfHealAttributedLeg {
    pub kind: RestartEventKind,
    pub cambox: String,
    pub since_ns: i64,
    pub reset_at_ns: i64,
    pub copies: u32,
    pub approx_stale_secs: f64,
    pub density: f64,
}

impl SelfHealAttributedLeg {
    /// The #895 acceptance-criteria operator message shape: distinctly labeled by its restart
    /// KIND (`self_heal_reset` / `capture_wedge` / `emit_freeze`), never reading as a camera
    /// fault. The `event_at` timestamp is the correlated restart event's own epoch-ns.
    pub fn message(&self) -> String {
        format!(
            "{}: {} since {} (event_at={}, copies={}, approx_stale_secs={:.1}, density={:.2}) -- {}",
            self.kind.label(),
            self.cambox,
            self.since_ns,
            self.reset_at_ns,
            self.copies,
            self.approx_stale_secs,
            self.density,
            self.kind.detail()
        )
    }
}

/// The combined report: genuinely-frozen windows (no correlating self-heal event), isolated
/// stale-replay windows (unaffected by this module, passed through from `classify_leg` verbatim),
/// self-heal-attributed windows (re-classified away from what would have been `frozen_leg`), and
/// every self-heal event observed during the run that did NOT correlate to any classified window
/// (still gates the run — see `any_self_heal`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SelfHealAttributionReport {
    pub frozen: Vec<FrozenLeg>,
    pub stale_replay: Vec<StaleReplayLeg>,
    pub self_heal: Vec<SelfHealAttributedLeg>,
    pub unattributed_events: Vec<SelfHealResetEvent>,
}

impl SelfHealAttributionReport {
    /// True the moment ANY window genuinely hard-froze with NO correlating self-heal reset.
    pub fn any_frozen(&self) -> bool {
        !self.frozen.is_empty()
    }

    /// True the moment a `#663` self-heal reset fired during the run at all — gates the run
    /// regardless of whether it also froze a window (the underlying rate defect is real and
    /// unresolved; a reset firing mid-measurement must never be silently waved through, #895
    /// acceptance criterion 4).
    pub fn any_self_heal(&self) -> bool {
        !self.self_heal.is_empty() || !self.unattributed_events.is_empty()
    }

    /// Whether this report's `frozen_leg`/`self_heal_reset` findings fold into the fused verdict's
    /// `overall_pass`. `any_frozen()`/`any_self_heal()` above are UNCHANGED regardless of this
    /// method's own state -- still fully computed, printed, and JSON-reported either way.
    ///
    /// RESTORED to blocking by issue 905 item 2 (2026-09-02): issue 914 (2026-08-01) had hardcoded
    /// this to `true` (report-only, never contributes a failure) while cam1's ShadowCast 2 grabber
    /// defect (issue 909) remained physically unresolved -- exactly mirroring the issue-889
    /// report-only pattern / issue-861's caller-only decoupling for `av_window::
    /// av_offset_gate_pass`. That restore condition (a stable green series with no self-heal
    /// escalations) is now met: three consecutive Full-path E2E runs showed a genuinely empty
    /// `frozen_leg.frozen` array AND a genuinely clean `self_heal_reset` (both `attributed` and
    /// `unattributed_events` empty) on every member. No env knob -- same no-knob discipline
    /// issue 889 established; this is a plain one-way flip, not a re-armable allowance.
    pub fn overall_pass_contribution(&self) -> bool {
        !self.any_frozen() && !self.any_self_heal()
    }
}

fn window_overlaps_event(start_ns: i64, end_ns: i64, at_ns: i64, margin_ns: i64) -> bool {
    at_ns >= start_ns - margin_ns && at_ns < end_ns + margin_ns
}

/// Classify every window in `segments` (the SAME classification `frozen_leg::frozen_leg_report`
/// uses), then re-attribute any HARD-FROZEN window whose span (padded by
/// [`SELF_HEAL_CORRELATION_MARGIN_NS`] either side) contains a same-camera self-heal reset event
/// to `self_heal_reset` instead of `frozen_leg`. Events not consumed by any window still gate the
/// run via `unattributed_events` (see `SelfHealAttributionReport::any_self_heal`). Each event
/// correlates to AT MOST one window (first match wins, `consumed` guards against double-counting
/// one reset across two overlapping-margin windows).
pub fn attribute_self_heal(
    segments: &[SegmentLeg<'_>],
    events: &[SelfHealResetEvent],
) -> SelfHealAttributionReport {
    let mut frozen = Vec::new();
    let mut stale_replay = Vec::new();
    let mut self_heal = Vec::new();
    let mut consumed = vec![false; events.len()];

    for s in segments {
        let window_secs = ((s.end_ns - s.start_ns).max(0) as f64) / 1_000_000_000.0;
        match classify_leg(s.copies, s.frames, window_secs) {
            LegHealth::Healthy => {}
            LegHealth::StaleReplay { copies } => stale_replay.push(StaleReplayLeg {
                cambox: s.cambox.to_string(),
                copies,
            }),
            LegHealth::Frozen {
                copies,
                approx_stale_secs,
                density,
            } => {
                let matched = events.iter().enumerate().find(|(i, e)| {
                    !consumed[*i]
                        && e.cambox == s.cambox
                        && window_overlaps_event(
                            s.start_ns,
                            s.end_ns,
                            e.at_ns,
                            SELF_HEAL_CORRELATION_MARGIN_NS,
                        )
                });
                if let Some((idx, ev)) = matched {
                    consumed[idx] = true;
                    self_heal.push(SelfHealAttributedLeg {
                        kind: ev.kind,
                        cambox: s.cambox.to_string(),
                        since_ns: s.start_ns,
                        reset_at_ns: ev.at_ns,
                        copies,
                        approx_stale_secs,
                        density,
                    });
                } else {
                    frozen.push(FrozenLeg {
                        cambox: s.cambox.to_string(),
                        since_ns: s.start_ns,
                        copies,
                        approx_stale_secs,
                        density,
                    });
                }
            }
        }
    }

    frozen.sort_by(|a, b| a.cambox.cmp(&b.cambox).then(a.since_ns.cmp(&b.since_ns)));
    stale_replay.sort_by(|a, b| a.cambox.cmp(&b.cambox));
    self_heal.sort_by(|a, b| a.cambox.cmp(&b.cambox).then(a.since_ns.cmp(&b.since_ns)));

    let unattributed_events: Vec<SelfHealResetEvent> = events
        .iter()
        .enumerate()
        .filter(|(i, _)| !consumed[*i])
        .map(|(_, e)| e.clone())
        .collect();

    SelfHealAttributionReport {
        frozen,
        stale_replay,
        self_heal,
        unattributed_events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_token() {
        let ev = SelfHealResetEvent::parse("cam1:1785439475449374588").unwrap();
        assert_eq!(ev.cambox, "cam1");
        assert_eq!(ev.at_ns, 1785439475449374588);
    }

    #[test]
    fn parse_rejects_empty_cambox() {
        assert!(SelfHealResetEvent::parse(":123").is_none());
    }

    #[test]
    fn parse_rejects_non_numeric_ns() {
        assert!(SelfHealResetEvent::parse("cam1:notanumber").is_none());
    }

    #[test]
    fn parse_rejects_missing_colon() {
        assert!(SelfHealResetEvent::parse("cam1_1785439475449374588").is_none());
    }

    #[test]
    fn no_events_behaves_like_frozen_leg_report() {
        let segments = vec![SegmentLeg {
            cambox: "cam5",
            copies: 300,
            frames: 9000,
            start_ns: 5_000_000_000,
            end_ns: 305_000_000_000, // 300s window -> approx_stale ~10s -> Frozen
        }];
        let report = attribute_self_heal(&segments, &[]);
        assert!(report.any_frozen());
        assert!(!report.any_self_heal());
        assert_eq!(report.frozen.len(), 1);
        assert_eq!(report.frozen[0].cambox, "cam5");
        assert!(report.self_heal.is_empty());
        assert!(report.unattributed_events.is_empty());
    }

    #[test]
    fn a_reset_inside_the_frozen_window_reattributes_away_from_frozen_leg() {
        // Mirrors #894's own live evidence shape: reset logged a few seconds into the affected
        // segment window.
        let segments = vec![SegmentLeg {
            cambox: "cam1",
            copies: 149,
            frames: 9000,
            start_ns: 1_785_439_470_000_000_000,
            end_ns: 1_785_439_500_000_000_000, // 30s window
        }];
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam1".to_string(),
            at_ns: 1_785_439_475_449_374_588, // ~5.4s into the window
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(
            !report.any_frozen(),
            "the window must be reattributed away from frozen_leg: {report:?}"
        );
        assert!(report.any_self_heal());
        assert_eq!(report.self_heal.len(), 1);
        assert_eq!(report.self_heal[0].cambox, "cam1");
        assert_eq!(report.self_heal[0].reset_at_ns, 1_785_439_475_449_374_588);
        assert!(report.unattributed_events.is_empty());
    }

    #[test]
    fn a_reset_on_a_different_camera_does_not_reattribute() {
        let segments = vec![SegmentLeg {
            cambox: "cam1",
            copies: 300,
            frames: 9000,
            start_ns: 0,
            end_ns: 30_000_000_000,
        }];
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam3".to_string(),
            at_ns: 15_000_000_000,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(
            report.any_frozen(),
            "a different camera's reset must NOT reattribute cam1's window"
        );
        assert_eq!(report.frozen.len(), 1);
        assert_eq!(report.frozen[0].cambox, "cam1");
        // The cam3 event never correlated to any window -> still gates the run.
        assert_eq!(report.unattributed_events.len(), 1);
        assert!(report.any_self_heal());
    }

    #[test]
    fn a_reset_just_outside_the_window_still_correlates_within_the_margin() {
        let segments = vec![SegmentLeg {
            cambox: "cam1",
            copies: 300,
            frames: 9000,
            start_ns: 10_000_000_000,
            end_ns: 40_000_000_000,
        }];
        // 2s before start_ns -- inside SELF_HEAL_CORRELATION_MARGIN_NS (5s).
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam1".to_string(),
            at_ns: 8_000_000_000,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(
            !report.any_frozen(),
            "a reset just before the window, within the margin, must still correlate"
        );
        assert_eq!(report.self_heal.len(), 1);
    }

    #[test]
    fn a_reset_beyond_the_margin_does_not_correlate() {
        let segments = vec![SegmentLeg {
            cambox: "cam1",
            copies: 300,
            frames: 9000,
            start_ns: 10_000_000_000,
            end_ns: 40_000_000_000,
        }];
        // 10s before start_ns -- beyond the 5s margin.
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam1".to_string(),
            at_ns: 0,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(
            report.any_frozen(),
            "a reset beyond the margin must not correlate"
        );
        assert_eq!(report.unattributed_events.len(), 1);
    }

    #[test]
    fn an_event_with_no_frozen_window_at_all_still_gates_via_unattributed() {
        // Healthy segment -> classify_leg never even reaches Frozen, but the self-heal event
        // itself must still be visible and still gate the run (#895 acceptance criterion 4).
        let segments = vec![SegmentLeg {
            cambox: "cam2",
            copies: 0,
            frames: 9000,
            start_ns: 0,
            end_ns: 30_000_000_000,
        }];
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam2".to_string(),
            at_ns: 15_000_000_000,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(!report.any_frozen());
        assert!(report.self_heal.is_empty());
        assert_eq!(report.unattributed_events.len(), 1);
        assert!(
            report.any_self_heal(),
            "a self-heal event must gate the run even with no correlating frozen window"
        );
    }

    #[test]
    fn stale_replay_windows_pass_through_unaffected() {
        let segments = vec![SegmentLeg {
            cambox: "cam2",
            copies: 2,
            frames: 9000,
            start_ns: 0,
            end_ns: 30_000_000_000,
        }];
        let report = attribute_self_heal(&segments, &[]);
        assert_eq!(report.stale_replay.len(), 1);
        assert_eq!(report.stale_replay[0].cambox, "cam2");
        assert!(!report.any_frozen());
        assert!(!report.any_self_heal());
    }

    #[test]
    fn two_reset_events_on_the_same_camera_each_correlate_to_their_own_window() {
        let segments = vec![
            SegmentLeg {
                cambox: "cam1",
                copies: 300,
                frames: 9000,
                start_ns: 0,
                end_ns: 30_000_000_000,
            },
            SegmentLeg {
                cambox: "cam1",
                copies: 300,
                frames: 9000,
                start_ns: 30_000_000_000,
                end_ns: 60_000_000_000,
            },
        ];
        let events = vec![
            SelfHealResetEvent {
                kind: RestartEventKind::SelfHealReset,
                cambox: "cam1".to_string(),
                at_ns: 15_000_000_000,
            },
            SelfHealResetEvent {
                kind: RestartEventKind::SelfHealReset,
                cambox: "cam1".to_string(),
                at_ns: 45_000_000_000,
            },
        ];
        let report = attribute_self_heal(&segments, &events);
        assert!(!report.any_frozen());
        assert_eq!(report.self_heal.len(), 2);
        assert!(report.unattributed_events.is_empty());
    }

    // ---- issue 905 item 2 (2026-09-02): frozen_leg/self_heal_reset RESTORED to blocking ----
    // (originally decoupled report-only by issue 914 on 2026-08-01, pending cam1's ShadowCast
    // grabber defect, issue 909; restored once a stable green E2E series showed both dimensions
    // genuinely clean -- frozen_leg.frozen==[] and self_heal_reset attributed/unattributed==[]
    // across three consecutive runs, see the ticket's own validated comment).

    #[test]
    fn a_genuinely_frozen_window_now_fails_overall_pass_905() {
        // Same fixture shape as `no_events_behaves_like_frozen_leg_report` above -- a genuinely
        // HARD-FROZEN window, no correlating self-heal event.
        let segments = vec![SegmentLeg {
            cambox: "cam5",
            copies: 300,
            frames: 9000,
            start_ns: 5_000_000_000,
            end_ns: 305_000_000_000, // 300s window -> approx_stale ~10s -> Frozen
        }];
        let report = attribute_self_heal(&segments, &[]);
        assert!(
            report.any_frozen(),
            "sanity: this fixture must genuinely classify Frozen: {report:?}"
        );
        assert!(
            !report.overall_pass_contribution(),
            "905: restored -- a genuinely frozen leg must fail overall_pass again: {report:?}"
        );
    }

    #[test]
    fn an_unattributed_self_heal_event_now_fails_overall_pass_905() {
        // Same fixture shape as `an_event_with_no_frozen_window_at_all_still_gates_via_unattributed`
        // above -- a self-heal reset that never correlates to any window.
        let segments = vec![SegmentLeg {
            cambox: "cam2",
            copies: 0,
            frames: 9000,
            start_ns: 0,
            end_ns: 30_000_000_000,
        }];
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam2".to_string(),
            at_ns: 15_000_000_000,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(
            report.any_self_heal(),
            "sanity: this fixture must genuinely trip any_self_heal: {report:?}"
        );
        assert!(
            !report.overall_pass_contribution(),
            "905: restored -- a self-heal reset event must fail overall_pass again: {report:?}"
        );
    }

    #[test]
    fn a_clean_report_also_contributes_no_failure() {
        let report = SelfHealAttributionReport::default();
        assert!(!report.any_frozen());
        assert!(!report.any_self_heal());
        assert!(
            report.overall_pass_contribution(),
            "a genuinely clean report must obviously never contribute a failure: {report:?}"
        );
    }

    #[test]
    fn message_shape_is_distinct_and_never_reads_as_a_camera_fault() {
        let leg = SelfHealAttributedLeg {
            kind: RestartEventKind::SelfHealReset,
            cambox: "cam1".to_string(),
            since_ns: 100,
            reset_at_ns: 103,
            copies: 149,
            approx_stale_secs: 5.31,
            density: 0.176,
        };
        let msg = leg.message();
        assert!(msg.starts_with("self_heal_reset: cam1 since 100"));
        assert!(msg.contains("NOT a frozen camera"));
    }

    // ---- issue 946 + issue 910: kind-tagged run-integrity restart events -----------------------
    // The capture-wedge (issue 945, exit 79) and emit-freeze (issue 944, exit 81) watchdogs each
    // force a restart whose gap produces exactly the stale/frozen frames a #663 USB reset does.
    // They correlate through the SAME engine, tagged with a distinct kind so the report reads
    // honestly ("capture_wedge restart", never "frozen camera").

    #[test]
    fn kind_label_round_trips() {
        for k in [
            RestartEventKind::SelfHealReset,
            RestartEventKind::CaptureWedge,
            RestartEventKind::EmitFreeze,
        ] {
            assert_eq!(RestartEventKind::from_label(k.label()), Some(k));
        }
        assert_eq!(RestartEventKind::from_label("nope"), None);
    }

    #[test]
    fn parse_legacy_untagged_token_defaults_to_self_heal_reset() {
        // The original #895 `--self-heal-reset cambox:epoch_ns` shape must keep working verbatim.
        let ev = SelfHealResetEvent::parse("cam1:1785439475449374588").unwrap();
        assert_eq!(ev.kind, RestartEventKind::SelfHealReset);
        assert_eq!(ev.cambox, "cam1");
        assert_eq!(ev.at_ns, 1785439475449374588);
    }

    #[test]
    fn parse_kind_tagged_capture_wedge_token() {
        let ev = SelfHealResetEvent::parse("capture_wedge:cam4:1785439475449374588").unwrap();
        assert_eq!(ev.kind, RestartEventKind::CaptureWedge);
        assert_eq!(ev.cambox, "cam4");
        assert_eq!(ev.at_ns, 1785439475449374588);
    }

    #[test]
    fn parse_kind_tagged_emit_freeze_token() {
        let ev = SelfHealResetEvent::parse("emit_freeze:cam2:100").unwrap();
        assert_eq!(ev.kind, RestartEventKind::EmitFreeze);
        assert_eq!(ev.cambox, "cam2");
        assert_eq!(ev.at_ns, 100);
    }

    #[test]
    fn parse_rejects_unknown_kind_label() {
        assert!(SelfHealResetEvent::parse("bogus_kind:cam1:100").is_none());
    }

    #[test]
    fn a_capture_wedge_event_reattributes_a_frozen_window_and_labels_it_distinctly() {
        let segments = vec![SegmentLeg {
            cambox: "cam4",
            copies: 149,
            frames: 9000,
            start_ns: 1_785_439_470_000_000_000,
            end_ns: 1_785_439_500_000_000_000, // 30s window
        }];
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::CaptureWedge,
            cambox: "cam4".to_string(),
            at_ns: 1_785_439_475_449_374_588,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(
            !report.any_frozen(),
            "a capture-wedge restart must reattribute the window away from frozen_leg: {report:?}"
        );
        assert!(report.any_self_heal());
        assert_eq!(report.self_heal.len(), 1);
        assert_eq!(report.self_heal[0].kind, RestartEventKind::CaptureWedge);
        let msg = report.self_heal[0].message();
        assert!(msg.starts_with("capture_wedge: cam4 since"), "{msg}");
        assert!(msg.contains("NOT a frozen camera"), "{msg}");
        assert!(
            msg.contains("945"),
            "must name the wedge watchdog issue: {msg}"
        );
    }

    #[test]
    fn an_emit_freeze_event_gates_even_with_no_correlating_window() {
        let segments = vec![SegmentLeg {
            cambox: "cam2",
            copies: 0,
            frames: 9000,
            start_ns: 0,
            end_ns: 30_000_000_000,
        }];
        let events = vec![SelfHealResetEvent {
            kind: RestartEventKind::EmitFreeze,
            cambox: "cam2".to_string(),
            at_ns: 15_000_000_000,
        }];
        let report = attribute_self_heal(&segments, &events);
        assert!(!report.any_frozen());
        assert_eq!(report.unattributed_events.len(), 1);
        assert_eq!(
            report.unattributed_events[0].kind,
            RestartEventKind::EmitFreeze
        );
        assert!(
            report.any_self_heal(),
            "an emit-freeze restart must still gate the run even with no correlating window"
        );
    }
}
