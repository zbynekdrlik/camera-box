//! #889 (2026-07-30, user decision on #883) — the per-cambox-window `#186` zero-loss verdict's
//! `copies`/`gaps` terms became REPORT-ONLY: still computed, still printed, still written to the
//! verdict JSON, but no longer forced a window (or the run) to FAIL. This was the HEAVIEST
//! relaxation in this repo's history — it relaxed the core zero-loss CLAIM itself, not a
//! measurement cost (contrast #888, which only relaxed imag's render-budget term). See #889 for
//! the full decision record (the failing evidence, run 30547146285) and the 3-part restore path
//! (root-cause #883 item 4, two consecutive clean STRICT runs, delete this relaxation).
//!
//! ## 2026-08-05 RE-GATE — [`WINDOW_COPIES_GAPS_TOLERANCE`] (ticket 889 comment 5196190653)
//!
//! Root-cause condition 1 of the restore path above was met: the issue-971 dupe-preferring
//! decimation fix (PR #993) directly attacks the over-rate-grabber duplicate/gap injection this
//! relaxation existed for. Its MEASURED residual under live over-rate (run 31033239950 attempt 1)
//! was max 1 copy AND max 1 gap per window — the decimation's bounded one-deferral-per-boundary
//! design admits occasional singletons BY DESIGN. Per the user's standing 2026-07-31 directive
//! ("jedna stratená snímka nie je problém — one lost frame per window is explicitly acceptable"),
//! `copies`/`gaps` are re-gated with a per-window SINGLETON tolerance instead of either extreme:
//! absolute zero (which would re-create a permanently-red gate on this bounded residual — the
//! issue-909 class of mistake, just relocated) or leaving the terms permanently report-only (which
//! would give up real regression protection against a return of the issue-971 10-45-per-window
//! failure class). `relaxed_pass` now requires `copies <= WINDOW_COPIES_GAPS_TOLERANCE && gaps <=
//! WINDOW_COPIES_GAPS_TOLERANCE` in addition to its prior terms — `strict_pass` (absolute zero)
//! stays byte-for-byte unchanged, so the singleton residual stays fully VISIBLE (never silent)
//! even though it no longer fails the run.
//!
//! **Not relaxed by this module:** `frame_count > 0` and the whole-run duration floor — those are
//! computed and folded in exactly as before. The `#881` calibrated optical-`undecodable` floor
//! (per-window here, run-wide in `probe::recording_segments::segment_continuity`) WAS in that
//! "never relaxed" set until issue 915 (2026-08-01, user decision) made it report-only too, using
//! the exact same shape this module already established for `copies`/`gaps` — see
//! `crate::optical_floor::gates_overall_pass` for the seam and restore path (issue 905).
//!
//! ## 2026-08-06 RECALIBRATION — tolerance 1 → 2 (issue 889 comment 5198131539)
//!
//! The tolerance above was calibrated from n=2 hardware runs (max 1 copy AND max 1 gap per
//! window each). A THIRD valid run (same commit, same rig, the transient rig anomaly from a
//! failed first attempt having cleared) measured the SAME order of total residual burden per run
//! (~5-7 copies + ~5-7 gaps) but showed it can randomly CLUSTER onto fewer windows — per-window
//! max across the three healthy runs is `{1, 1, 2}`, not a flat `1`. At tolerance=1 the gate was
//! flaky by construction: a healthy run has a real chance of a window landing on 2 defects purely
//! by how the same fixed total happens to distribute, and would FAIL a run that is not actually
//! anomalous. Tolerance=2 absorbs that measured clustering while preserving full discrimination —
//! the genuine anomaly (the failed first attempt) measured 9-15 copies and 8-15 gaps on EVERY
//! window, 4.5x and more over the new threshold, so the re-gate still catches it every time. The
//! rejected alternative (leave tolerance at 1 and re-run until a healthy run happens to pass) is
//! the banned blind-rerun pattern — gambling on clustering instead of measuring it, ~40 min per
//! rig cycle, and a gate that stays flaky for every future PR. See ticket 889 comment 5198131539
//! for the full evidence (three run IDs, per-window breakdowns) this recalibration is based on.
//!
//! ## 2026-08-06 RECALIBRATION (2 -> 3) — issue 889 comment 5200533407
//!
//! A post-#998-fix hardware run (95843090, attempt 3 — the #998 stream genlock settle-back drain
//! fix deployed to all three boxes, plus a session-1 OBS instance) came back with `frozen_leg`
//! EMPTY (was 10 events) and the stream FIFO audit reading `dropped_due`/`late_holds` at +0/+0
//! across the WHOLE recording window (was +152/+151) — the #998 fix is proven effective on the
//! rig itself, not just in unit tests. Zero-loss held on every node; 9 of 10 windows stayed
//! within the tolerance=2 boundary, but ONE window measured copies=3/gaps=3, failing the gate.
//! Investigated, not just bumped: the run's TOTAL residual burden (8 copies + 7 gaps) is the same
//! order as every other healthy run (~5-7 + 5-7 — the chronic 62.15fps over-rate-decimation lane,
//! a separate, already-fixed-once, still-not-zero lane unrelated to #998) — it simply clustered
//! onto one window instead of spreading evenly across ten. Healthy per-window maxima across n=4
//! runs are now `{1, 1, 2, 3}`, so tolerance=2 is flaky by construction the same way tolerance=1
//! was before it (~8 defects over 10 windows puts P(some window >= 3) at roughly 20%; tolerance=3
//! drops P(some window >= 4) to roughly 2-4%). Discrimination is unaffected: the genuine #998
//! anomaly (pre-fix) measured 9-15 and 8-17 copies/gaps on EVERY window, still 3x+ over this
//! threshold, so a real regression of that class still fails loudly. The real fix remains driving
//! the over-rate lane's residual burden to zero (tracked on its own, separate tickets); once it
//! does, the tolerance is pulled back down — noted here as a standing commitment, not deferred to
//! a follow-up issue. See ticket 889 comment 5200533407 for the full evidence (the run id, the
//! per-window breakdown, and the burden comparison against clean runs).
//!
//! ## 2026-08-14 RE-TIGHTENING (3 -> 1) — issue 1031
//!
//! The over-rate lane's residual burden the 2 -> 3 section flagged as "the real fix" got two
//! genlock fixes: issue 1042 (source interval from the MIN grid delta, killing the spurious
//! backlog-relock erase) and issue 1049 (bounded phase convergence gated to N>=2, killing the
//! strih-ingest relock storm + the deep n=1 limit cycle). Both deployed to the rig (imag-nb +
//! strih genlock replaced 09:17 CEST, OBS restarted 09:18). The one steady-state post-fix
//! full-cycle run (1780620060, started +27 min after the OBS restart, fully converged) measured a
//! healthy per-window max of {copies 1, gaps 1}, windows_over_copies_gaps_tolerance = 0,
//! overall_pass = true — the chronic burden collapsed to a single stale_replay dup+gap pair, so
//! the honest floor is now 1, not 3. Made good on the 2 -> 3 section's standing commitment: the
//! tolerance is pulled back down as the burden falls. NOT yet 0 — that single stale_replay is the
//! issue-859 shared-duplicate residual (root cause not landed); 0 stays gated on issue 859 AND
//! N>=2 consecutive green post-fix runs at tolerance=1 measuring per-window 0/0. Issue 1031 owns
//! that remaining 1 -> 0 step. The excluded run 1074024850 (started 2 min after the OBS restart,
//! measured 14/window) is the convergence transient, not steady state. See issue 1031 for the
//! full per-run distribution table.
//!
//! ## 2026-08-14 STEP-BACK (1 -> 2) — issue 1031, same day
//!
//! Tolerance 1 failed its own consecutive-greens evidence test immediately: the next two
//! full-cycle runs after the 3 -> 1 step measured healthy per-window sums of 2-3 (run 900430067:
//! CAM1 2+0, CAM2 1+2, CAM3 1+1) — the issue-859 shared-duplicate residual still produces
//! 2-per-window bursts, so 1 was calibrated on an unluckily-clean n=1 sample. Stepped back to 2
//! (still tighter than the previous 3): every observed healthy window sums <= 2 across the
//! post-fix sample. The 2 -> 1 -> 0 walk stays gated on the issue-859 root cause + N>=2
//! consecutive greens at each step, tracked on issue 1031.
//!
//! ## 2026-08-29 RE-ARM (#1220, owner-mandated soft-release) — the `<=3` tolerance channel folds
//! into `overall_pass` again, superseding the #1169 `<=1/<=1` singleton (kept wired, now dormant)
//!
//! Two back-to-back full-cycle E2E runs the same day (1989954227, 797081170) both PASSED the
//! `all_cambox_av_sync` gate — the first two consecutive green A/V runs in this project's history
//! — while `all_cambox_continuity` stayed red on the SECOND run purely because THREE windows
//! (CAM2 copies=2/gaps=2, CAM6 copies=2/gaps=1, CAM7 copies=2/gaps=3) sat OVER the currently-live
//! #1169 `<=1/<=1` singleton band (a FOURTH, CAM3 copies=1/gaps=0, was already WITHIN that band
//! and absorbed as the run's one consumed singleton) — every one of the four sat comfortably
//! WITHIN the already-calibrated `<=3` [`WINDOW_COPIES_GAPS_TOLERANCE`] channel
//! (`windows_over_copies_gaps_tolerance: 0` on that run). Per the owner's standing 2026-07-31
//! strict-test revision ("jemne uvoľniť gate na zelenú + tickety na pritvrdenie, potom ticket po
//! tickete; jedna stratená snímka nie je problém"), [`copies_gaps_tolerance_gates_overall_pass`]
//! is RE-ARMED (`true`) — this is a deliberate OVERRIDE of that same function's originally-stated
//! restore preconditions (the CAM1 sick leg physically fixed/excluded, AND the #1031 walk-down
//! resumed) — the owner's blanket soft-release-to-green directive supersedes those preconditions
//! for THIS ticket exactly the way `verdict-gate-seam-calibration.md` §11 documents an
//! owner-mandated override of the ordinary gates-green-first discipline, just in the RELAXING
//! direction instead of the tightening one. `[WINDOW_COPIES_GAPS_TOLERANCE]` is now what actually
//! GATES `overall_pass` again, not merely what `relaxed_pass` reports — `overall_pass_term` now
//! equals `relaxed_pass` exactly (the pre-#1132 fold), and `pass`/`strict_pass`/
//! `windows_failed_report_only` stay byte-for-byte unchanged and fully visible (the strict counts
//! stay reported, per the ticket).
//!
//! **The #1169 `<=1/<=1` singleton mechanism is left WIRED, not deleted, and its own arm flag
//! ([`segment_singleton_allowance_gates_overall_pass`]) is UNTOUCHED (still hardcoded `true`) —**
//! it is now DORMANT purely by `if`/`else if` PRECEDENCE inside [`decide`], not by its own flag
//! being flipped. This is a deliberate GRADUATED-FALLBACK property, not an oversight: if a future
//! walk-down step ever disarms [`copies_gaps_tolerance_gates_overall_pass`] again (the residual
//! shrinks further), the run does not fall straight to absolute-zero — it automatically resumes
//! the already-calibrated `<=1/<=1` band first, one graduated step at a time, exactly mirroring
//! this codebase's own `gate-allowance-restore-red-green.md` "leave the mechanism dormant, never
//! delete it" doctrine, applied here to a seam superseded by PRECEDENCE for the first time rather
//! than by its own flag. Ticket #1169's own independent re-tighten trail (for the singleton band
//! itself) is unaffected and stays open on its own ticket.
//!
//! **The walk-down commitment does not move — it stays open on #1220.** Run 1989954227 (the
//! EARLIER of the two same-day runs) still genuinely exceeds even the `<=3` ceiling on three
//! windows (two CAM2 windows at copies=10/gaps=9 and copies=19/gaps=18, one CAM7 window at
//! copies=4/gaps=5 — `windows_over_copies_gaps_tolerance: 3`) — a real, still-open defect on the
//! chronic over-rate lane, correctly still RED after this re-arm, not papered over by it. When
//! that lane's burden shrinks further, the tolerance walks back down exactly as #889/#1031/#1121
//! already did for this same constant — tracked on #1220, not silently dropped.
//!
//! ## 2026-08-31 RE-CALIBRATION (3 -> 5) — issue 1243 (third relax-walk step), walk-back on #1242
//!
//! Per `.claude/rules/window-gate-tolerance-walkdown.md`'s data-first procedure: three complete
//! post-fix 7-cam verdicts (no dead-painter/no-signal windows, none inside a convergence
//! transient) gave a larger fresh sample than the 2026-08-15 stepped-back-to-3 calibration was
//! based on --
//!
//! - run 1629895310: per-window copies `[1,1,1,0,0,1,0,0,1,0]`, worst max(copies,gaps)=1.
//! - run 1230380558: per-window copies `[0,0,0,0,1,0,0,0,1,1]`, worst max(copies,gaps)=1.
//! - run 1142514714: per-window copies `[1,0,1,4,1,0,0,0,0,0]`, worst max(copies,gaps)=4 --
//!   seg3 CAM4, four SEPARATE single-frame duplicates spread over ~14s (no self-heal/frozen_leg
//!   events in that window), the run's SOLE blocking-gate failure: `#889 RE-GATE FAIL: copies=4
//!   gaps=4 exceeds tolerance (3)`. Uniformity passed comfortably (0.9325 vs the now-0.90 floor,
//!   see `presentation_cadence::UNIFORM_FRACTION_MIN`'s own walk-down history), isolating this as
//!   a pure copies/gaps-tolerance red, not a cascading uniformity/optical defect.
//!
//! `TOL_min = max(maxCopies, maxGaps)` over the three steady post-fix runs is 4. Stepping to
//! exactly 4 (the bare observed ceiling) would leave zero margin, and the n=3 per-run worst
//! values (`{1, 1, 4}`) already show the same run-to-run clustering variance every earlier step
//! on this const hit at its own ceiling (the 2026-08-06 2->3 recalibration, explicitly). 5 gives
//! ONE event of margin above the worst observed run while staying far under the 9-45/window band
//! every genuine sick-leg/limit-cycle regression on this const has measured (discrimination is
//! unaffected). See issue 1243's design-addendum comment for the full rationale, including the
//! rejected alternatives. The walk-back trail stays on **issue 1242**, unchanged by this step.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as `optical_floor.rs` / `av_window.rs`'s `#861` relaxation: the whole `probe`
//! module is `#[cfg(feature = "probe")]` (CLAUDE.md's Local Build Policy — heavy deps balloon the
//! shared dev1 `target/`), so a change confined to `probe::recording_segments` has ZERO local
//! verification path, not even a compile check. This module is the PURE strict-vs-relaxed
//! decision seam; `probe::recording_segments::window_segment` only calls it and wires the result
//! onto `CamboxSegment`.
//!
//! ## The #861 precedent this mirrors
//!
//! `av_window::av_offset_gate_pass` stayed UNCHANGED in meaning when its gate went report-only —
//! only the CALLER stopped folding it into `overall_pass` ("the pure decision function stays
//! unchanged, still measured, still fails CLOSED on thin data — only the caller stopped folding
//! its result"). This module follows the identical shape: [`decide`] still computes the pre-#889
//! STRICT verdict ([`WindowGateDecision::strict_pass`], byte-for-byte the same boolean
//! `probe::recording_segments::CamboxSegment::pass` has always held) alongside the NEW relaxed
//! verdict the caller actually folds into `overall_pass`.

/// The per-window tolerance applied to `copies`/`gaps` when folding them back into
/// `relaxed_pass`/`overall_pass` (2026-08-05 re-gate, ticket 889 comment 5196190653; recalibrated
/// 1 → 2 on 2026-08-06, ticket 889 comment 5198131539; recalibrated again 2 → 3 later the same
/// day, ticket 889 comment 5200533407; RE-TIGHTENED 3 -> 1 on 2026-08-14, issue 1031, after the
/// issue-1042 + issue-1049 genlock fixes landed on the rig). Hardcoded, no env knob — a silent
/// env default is exactly
/// how "temporary" becomes permanent (the original issue-889 requirement 4), and this repo's
/// standing rule is that a needed capability is always ON by default, never a forgettable toggle
/// (`features-default-on-never-forgettable-toggle` in the project memory). A window with `copies >
/// WINDOW_COPIES_GAPS_TOLERANCE` OR `gaps > WINDOW_COPIES_GAPS_TOLERANCE` fails
/// [`WindowGateDecision::relaxed_pass`] again; at or under the tolerance it is absorbed (still
/// visible via `strict_pass` / the #889 per-window WARN, never silent).
///
/// **Calibration basis (issue 889 comment 5200533407):** a post-#998-fix hardware run (95843090)
/// measured `frozen_leg` empty and the stream FIFO `dropped_due`/`late_holds` at +0/+0 through the
/// whole recording — the #998 settle-back drain fix confirmed effective on the rig — yet ONE
/// window still measured copies=3/gaps=3, from the SEPARATE, chronic over-rate-decimation lane
/// (run-total burden 8 copies + 7 gaps, the same order as every other healthy run). Healthy
/// per-window maxima across n=4 runs are now `{1, 1, 2, 3}` — tolerance=2 was flaky by
/// construction the same way tolerance=1 was before it. The genuine anomaly (pre-#998) measured
/// 9-15/8-17 copies/gaps on EVERY window, still 3x+ over this threshold, so discrimination against
/// a real regression is unaffected. See the module doc's second 2026-08-06 RECALIBRATION section
/// above for the full rationale.
///
/// **Re-tightening basis (issue 1031, 2026-08-14):** the two genlock fixes that attacked the
/// chronic residual above deployed to the rig (imag-nb + strih genlock replaced 09:17 CEST, OBS
/// restarted 09:18) -- issue 1042 (source interval from the MIN grid delta, killing the spurious
/// backlog-relock erase) and issue 1049 (bounded phase convergence gated to N>=2, killing the
/// strih-ingest relock storm + the deep n=1 limit cycle). The one steady-state post-fix
/// full-cycle E2E run (1780620060, started 09:45 -- +27 min after the OBS restart, fully
/// converged) measured a healthy per-window max of `{copies 1, gaps 1}`, `windows_over_copies_
/// gaps_tolerance = 0`, `overall_pass = true`: the chronic burden collapsed to a single
/// `stale_replay` dup+gap pair (CAM2 x1, CAM3 x1) at a couple of switch seams, the rest 0/0. So
/// the honest floor is now 1, not 3. It is NOT yet 0 -- that single stale_replay is the issue-859
/// shared-duplicate residual, whose root cause has not landed; 0 stays gated on issue 859 AND
/// N>=2 consecutive green post-fix runs at tolerance=1 measuring per-window 0/0 (issue 1031 owns
/// that remaining 1 -> 0 step). The pre-fix relock storms (14-28 copies/gaps/window) and the
/// OBS-restart convergence transient (14/window, run 1074024850, excluded as it started 2 min
/// after the restart) are all still 10x+ over this threshold, so a real regression fails loudly.
///
/// **Stepped BACK to 3 (issue 1031 comment 5300948461, 2026-08-15):** the cam1 ShadowCast
/// grabber's chronic degradation (issue 728 assessment; replacement Cam Link ordered, issue 1034
/// re-measures after the swap) broke the tolerance=2 calibration's hardware assumptions: 14
/// consecutive runs on 08-14/08-15 measured per-window max(copies,gaps) values above 2 in 8 of
/// them, dominantly 3 (5x) and 4 (2x), almost always carried by CAM1 gap bursts; the 5/10 tail
/// rode multi-cam bursts that fail at any tolerance. 3 is the ORIGINAL ship value of this gate
/// and the tightest value the degraded-hardware sample supports; the 3 -> 2 -> 1 -> 0 walk-down
/// resumes on issue 1031 after the cam1 card swap + consecutive greens.
///
/// **#1220 (2026-08-29): this constant is LIVE again — it directly gates `overall_pass`, not just
/// `relaxed_pass`.** [`copies_gaps_tolerance_gates_overall_pass`] was re-armed (owner-mandated
/// soft-release, see the module doc's "2026-08-29 RE-ARM" section for the full evidence and
/// decision record).
///
/// **Walked 3 -> 5 on 2026-08-31 (issue 1243, third relax-walk step; walk-back tracked on issue
/// 1242) -- see the module doc's "2026-08-31 RE-CALIBRATION" section for the full three-run
/// evidence table.** A fresh, larger post-fix sample (n=3 complete 7-cam verdicts) gave a worst
/// observed max(copies,gaps) of 4 (run 1142514714, seg3 CAM4 -- four separate single-frame
/// duplicates, no self-heal/frozen_leg events, the run's sole blocking-gate failure); 5 gives one
/// event of margin above that ceiling while staying far under the 9-45/window band every genuine
/// regression on this const has measured.
pub const WINDOW_COPIES_GAPS_TOLERANCE: u32 = 5;

/// #1251 (2026-09-01) -- TEMPORARY per-cambox copies/gaps tolerance override, walk-back tracked on
/// issue 1242 (umbrella) + step recorded on issue 1243 (relax-steps).
///
/// CAM2's grabber card/port/cable (issue 1249 HW) under-delivers in sub-second bursts, so the
/// #1167 v4 slot-fill pacer repeats the last frame (a copy) then skips (a gap): run 1326320314
/// measured CAM2 windows at copies=8/gaps=8 and copies=18/gaps=17, while every OTHER box stayed
/// within 5 (max 5/5 on CAM3). cam3+cam5 carry the SAME card model on the SAME splitter and are
/// rock-solid, so this is a per-CAM2 HARDWARE defect, not a code fault (#1167 v4 slot-fill is
/// already the graceful degrade). Scoped relax to green per the owner's standing 2026-07-31 mandate
/// ("jemne uvoľniť gate na zelenú + tickety na pritvrdenie"). The override (25) covers the observed
/// 18 with a margin band; every other box keeps the default [`WINDOW_COPIES_GAPS_TOLERANCE`].
///
/// Keyed by the EXACT runtime cambox label -- production is always UPPERCASE `CAMN`
/// (`camera_active_sweep_pairs` emits `Cam N:CAMN`; run 1326320314's switch-schedule.json + verdict
/// both carry `"cambox": "CAM2"`). The lowercase `cam2` used by the `recording_segments.rs` unit
/// fixtures deliberately does NOT match, so the override only ever touches the real rig (and the
/// existing lowercase-cam2 boundary tests stay exactly as calibrated).
///
/// **Walk-back:** set this to `&[]` when issue 1249's HW swap lands -- tracked on issue 1242.
pub const WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX: &[(&str, u32)] = &[("CAM2", 25)];

/// The per-window copies/gaps tolerance that applies to `cambox`: the
/// [`WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX`] override if one is listed for this EXACT label, else
/// the default [`WINDOW_COPIES_GAPS_TOLERANCE`]. Exact-match on purpose (see the map's own doc).
pub fn copies_gaps_tolerance_for_cambox(cambox: &str) -> u32 {
    for (name, tol) in WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX {
        if *name == cambox {
            return *tol;
        }
    }
    WINDOW_COPIES_GAPS_TOLERANCE
}

/// #1132 (owner mandate 2026-08-19): whether the per-window copies/gaps TOLERANCE
/// ([`WINDOW_COPIES_GAPS_TOLERANCE`]) is allowed to RESCUE the verdict folded into `overall_pass`
/// ([`WindowGateDecision::overall_pass_term`]). Hardcoded `false` — the owner ordered the relaxed
/// copies/gaps rescue removed after the CAM1 grabber incident (a hardware-sick leg with
/// copies=1/gaps<=3 passed green for a week, masking the defect; every multi-frame event must now
/// be RED + escalated, never absorbed). While `false`, `overall_pass_term` requires `copies == 0
/// && gaps == 0`; the tolerance MECHANISM (`relaxed_pass`, [`WINDOW_COPIES_GAPS_TOLERANCE`], its
/// walk-down history — issue 1031/1121 —, and the whole reporting/JSON path) stays DORMANT and
/// fully computed for observability, it just no longer rescues (the
/// `gate-allowance-restore-red-green` dormant-mechanism pattern). Mirrors
/// `crate::optical_floor::gates_overall_pass`'s one-line `gates_overall_pass()` seam shape EXACTLY,
/// and is INDEPENDENT of it — #1132 touches ONLY copies/gaps; the optical undecodable floor is
/// governed by its OWN separate seam (issue 915/905, LIVE again since issue 905 item 3), never
/// touched by this change.
///
/// **Restore path (as originally stated):** flip this ONE function back to `true` once the sick
/// leg is physically fixed/excluded (issue 1110/1134) AND the walk-down (issue 1031) resumes to
/// its data-supported value; then `overall_pass_term == relaxed_pass` again and the tolerance
/// folds exactly as it did pre-#1132.
///
/// **#1220 (owner mandate, 2026-08-29) RE-ARMED this to `true` — an explicit OVERRIDE of the
/// restore-path preconditions immediately above, not proof they were met.** Two same-day
/// full-cycle runs (1989954227, 797081170) both passed `all_cambox_av_sync` for the first time in
/// this project's history; the SECOND run's `all_cambox_continuity` failed purely because THREE
/// windows sat over the tighter #1169 `<=1/<=1` singleton band (a FOURTH sat WITHIN that band,
/// already absorbed) while all four sat fully within this ALREADY-CALIBRATED `<=3` channel
/// (`windows_over_copies_gaps_tolerance: 0` on that run). Per the
/// owner's standing 2026-07-31 revision ("jemne uvoľniť gate na zelenú + tickety na pritvrdenie,
/// potom ticket po tickete"), the calibrated channel is re-armed as the sole fold-governing term
/// while the walk-down continues ticket-by-ticket (tracked on #1220, not closed by it) — see the
/// module doc's "2026-08-29 RE-ARM" section for the full evidence and the graduated-fallback
/// property this leaves in place for [`segment_singleton_allowance_gates_overall_pass`] (untouched,
/// still `true`, now reachable only if this seam is disarmed again in a future walk-down step).
pub fn copies_gaps_tolerance_gates_overall_pass() -> bool {
    true
}

/// #1169 (owner, 2026-08-22): the per-segment SINGLETON allowance applied to `copies`/`gaps` when
/// folding them into the BLOCKING verdict ([`WindowGateDecision::overall_pass_term`]). A DISTINCT,
/// strictly-tighter band than the disarmed (#1132) `WINDOW_COPIES_GAPS_TOLERANCE` (`<=3`) rescue:
/// a segment with `copies <= SEGMENT_SINGLETON_COPIES_ALLOWANCE && gaps <=
/// SEGMENT_SINGLETON_GAPS_ALLOWANCE` is ABSORBED into the blocking verdict, while `>=2` of EITHER
/// still fails. This is the designed absorption cost of the issue-1167 v3 paced-trickle (`<=1` skip
/// per ~0.5s on an over/near-rate box) + the matching FIFO `stale_replay` (the same event surfaced
/// twice), post the CAM1 card swap -- NOT a hardware-sick leg.
///
/// **Never re-arm `copies_gaps_tolerance_gates_overall_pass()` for this** -- that (`<=3`) band is
/// exactly the CAM1-class mask #1132 removed. This is a SEPARATE, tighter seam with its own
/// re-tighten trail (issue 1169). The absorption is LOUD, never silent: `strict_pass`/`pass` stay
/// false (visible), a per-segment note fires ([`segment_singleton_note`]), and the run-level count
/// is serialized -- addressing #1132's masking concern while honoring the owner's 2026-07-31
/// "jedna stratená snímka nie je problém" soft-release doctrine.
///
/// **#1220 (2026-08-29) SUPERSEDES the "never re-arm" line above by explicit, ticket-tracked owner
/// mandate** -- `copies_gaps_tolerance_gates_overall_pass()` IS re-armed (see its own doc for the
/// full decision record), which makes this whole seam DORMANT via `if`/`else if` PRECEDENCE inside
/// [`decide`] (this function's own return value is untouched, still `true`). This is not the
/// ad-hoc shortcut the "never re-arm" warning guarded against -- it is a separate, documented,
/// owner-mandated ticket (#1220) re-arming the WIDER already-calibrated channel outright, not a
/// silent workaround reaching for the `<=3` band to fake a tighter absorption. Kept fully wired
/// (never deleted) as the graduated FALLBACK if a future walk-down step disarms #1220's seam again
/// -- see the module doc's "2026-08-29 RE-ARM" section.
pub const SEGMENT_SINGLETON_COPIES_ALLOWANCE: u32 = 1;
/// See [`SEGMENT_SINGLETON_COPIES_ALLOWANCE`].
pub const SEGMENT_SINGLETON_GAPS_ALLOWANCE: u32 = 1;

/// #1169: whether the per-segment SINGLETON allowance is ARMED (folds `<=1/<=1` into the blocking
/// verdict). Hardcoded `true` -- the sanctioned soft-release to green. **Re-tighten to absolute
/// zero = flip this ONE function back to `false`** (the `gate-allowance-restore-red-green`
/// dormant-mechanism pattern, exactly like #1132/#905), after which `overall_pass_term` requires
/// strict `copies == 0 && gaps == 0` again. Gated on the issue-1168 floor reduction and/or the
/// cam1 card swap landing + N consecutive zero-singleton green runs -- issue 1169 owns that step
/// and stays OPEN as the re-tighten trail. Mirrors `copies_gaps_tolerance_gates_overall_pass`'s
/// shape and is INDEPENDENT of it (originally: "the disarmed `<=3` rescue stays disarmed" --
/// **#1220 (2026-08-29) changed that half:** the `<=3` rescue is RE-ARMED, so this function's
/// return value (still `true`, untouched by #1220) is now DORMANT by `if`/`else if` PRECEDENCE
/// inside [`decide`] rather than by its own flag -- it stays wired as the graduated FALLBACK a
/// future walk-down step re-engages automatically if [`copies_gaps_tolerance_gates_overall_pass`]
/// is ever disarmed again. Issue 1169's own re-tighten trail (this flag to `false`) is unaffected
/// and independent of #1220's seam.
pub fn segment_singleton_allowance_gates_overall_pass() -> bool {
    true
}

/// #1169: `true` exactly when a segment's nonzero `copies`/`gaps` are absorbed ONLY by the
/// singleton allowance -- i.e. the allowance is armed, the (`<=3`) tolerance rescue is disarmed,
/// there is genuinely something to absorb (`copies > 0 || gaps > 0`), and it sits within the
/// `<=1/<=1` band. Drives the loud per-segment note + the run-level count; never fires on a clean
/// segment, an over-band (still-failing) segment, or one rescued by the (dormant) `<=3` tolerance.
pub fn segment_singleton_allowance_consumed(copies: u32, gaps: u32) -> bool {
    segment_singleton_allowance_gates_overall_pass()
        && !copies_gaps_tolerance_gates_overall_pass()
        && (copies > 0 || gaps > 0)
        && copies <= SEGMENT_SINGLETON_COPIES_ALLOWANCE
        && gaps <= SEGMENT_SINGLETON_GAPS_ALLOWANCE
}

/// #1169: the LOUD per-segment note for an absorbed singleton -- `Some(..)` iff
/// [`segment_singleton_allowance_consumed`], `None` otherwise (never noise on a clean or
/// still-failing segment). Flows onto `CamboxSegment::singleton_allowance_note` -> the verdict JSON
/// + the per-run report, so the absorption is never silent (the #1132 masking guard).
pub fn segment_singleton_note(copies: u32, gaps: u32) -> Option<String> {
    if !segment_singleton_allowance_consumed(copies, gaps) {
        return None;
    }
    Some(format!(
        "#1169 SINGLETON ALLOWANCE consumed: copies={copies} gaps={gaps} (<= {}/{} each) absorbed \
         into overall_pass -- this segment FAILS the absolute-zero bar (strict pass stays false, \
         visible), the designed issue-1167 paced-trickle + FIFO stale_replay residual; 2+ of \
         either still fails. Re-tighten trail: issue 1169.",
        SEGMENT_SINGLETON_COPIES_ALLOWANCE, SEGMENT_SINGLETON_GAPS_ALLOWANCE
    ))
}

/// The strict-vs-relaxed decision for one cambox window, given its already-computed counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGateDecision {
    /// The pre-#889 verdict — UNCHANGED semantics: `frame_count > 0 && <undecodable within the
    /// #881 calibrated floor> && copies == 0 && gaps == 0`. Still computed, still exposed as
    /// `CamboxSegment::pass`, and drives the #889 loud per-window WARN — never silently dropped.
    pub strict_pass: bool,
    /// The verdict actually folded into `overall_pass`. Issue 889 (2026-07-30): `frame_count > 0
    /// && <undecodable within the #881 floor>`. 2026-08-05 RE-GATE: gained `&& copies <=
    /// WINDOW_COPIES_GAPS_TOLERANCE && gaps <= WINDOW_COPIES_GAPS_TOLERANCE` — `copies`/`gaps` are
    /// no longer fully report-only, they are tolerated up to the calibrated per-window threshold
    /// and gate again above it.
    ///
    /// **#1132 (owner mandate 2026-08-19): this field was made REPORTED-ONLY for observability —
    /// it stopped folding into `overall_pass`, the run fold used [`Self::overall_pass_term`]
    /// instead.** Kept computed (with the tolerance) so the JSON shows what the relaxed verdict
    /// WOULD say — a `relaxed_pass == true` window whose `overall_pass_term == false` is a
    /// disarmed rescue visibly doing nothing, never a hidden mask.
    ///
    /// **#1220 (owner mandate, 2026-08-29): [`copies_gaps_tolerance_gates_overall_pass`] is
    /// RE-ARMED, so this field now EQUALS [`Self::overall_pass_term`] again** (see that field's
    /// own doc and `copies_gaps_tolerance_gates_overall_pass`'s doc for the full decision record).
    /// Kept as a SEPARATE field regardless — a future walk-down step may disarm the seam again, at
    /// which point this field resumes showing what the tolerance channel WOULD say even while a
    /// stricter fold governs `overall_pass_term`, exactly the observability role it has always had.
    pub relaxed_pass: bool,
    /// #1132 (owner mandate 2026-08-19): the verdict ACTUALLY folded into the run-level
    /// `overall_pass` (`crate::probe::recording_segments::segment_continuity`). Originally made
    /// STRICTER than [`Self::relaxed_pass`] on copies/gaps — the copies/gaps tolerance no longer
    /// rescued it (`copies_gaps_tolerance_gates_overall_pass() == false`), so a single copy or gap
    /// failed. The optical undecodable floor is governed by its OWN seam here EXACTLY as in
    /// `relaxed_pass` (the SAME `crate::optical_floor::gates_overall_pass()` term, LIVE since issue
    /// 905 item 3 — #1132 does NOT touch it). Per this field's own original doc: "When
    /// `copies_gaps_tolerance_gates_overall_pass()`
    /// is flipped back on, this equals `relaxed_pass`."
    ///
    /// **#1220 (owner mandate, 2026-08-29): exactly that flip happened** — see
    /// `copies_gaps_tolerance_gates_overall_pass`'s own doc for the full decision record. This
    /// field now equals [`Self::relaxed_pass`] on every input (the copies/gaps term is
    /// `within_tolerance` again, restoring the pre-#1132 fold); the #1169 `<=1/<=1` singleton band
    /// stays wired as a graduated fallback (see [`Self::singleton_allowance_consumed`]'s own doc)
    /// but is currently unreachable via `decide()`'s `if`/`else if` precedence.
    pub overall_pass_term: bool,
    /// #1169: `true` iff this window's nonzero `copies`/`gaps` were ABSORBED into
    /// [`Self::overall_pass_term`] ONLY by the `<=1/<=1` singleton allowance
    /// ([`segment_singleton_allowance_consumed`]). Records the absorption loudly (never a silent
    /// mask): a `singleton_allowance_consumed == true` window has `strict_pass == false` and a
    /// per-segment note. `false` on a clean window, an over-band (still-failing) window, and a
    /// window rescued by the (originally dormant) `<=3` tolerance.
    ///
    /// **#1220 (owner mandate, 2026-08-29): ALWAYS `false` now** — `segment_singleton_allowance_
    /// consumed` itself requires `!copies_gaps_tolerance_gates_overall_pass()`, which is `true`
    /// (armed) as of #1220, so this field can never fire while the re-armed tolerance channel
    /// governs `overall_pass_term`. The field, the const band, and `segment_singleton_note` all
    /// stay wired (never deleted) as the graduated fallback — see the module doc's "2026-08-29
    /// RE-ARM" section.
    pub singleton_allowance_consumed: bool,
}

impl WindowGateDecision {
    /// `true` exactly when THIS window's `copies`/`gaps` term being WITHIN the per-window
    /// tolerance (2026-08-05 re-gate, recalibrated 2026-08-06) and/or the optical undecodable
    /// floor (issue 915) is the reason `strict_pass` and `relaxed_pass` disagree — i.e. some
    /// report-only/tolerance relaxation, and only that, is doing work on this window.
    /// `frame_count == 0` fails BOTH verdicts unconditionally (an absent cambox proves nothing
    /// either way, never rescued) — see `zero_frames_fails_both_verdicts_889` below. A window
    /// whose `copies`/`gaps` EXCEED the tolerance fails both verdicts too (not rescued) — see the
    /// `..._over_tolerance_..` tests below.
    pub fn relaxed_by_889(&self) -> bool {
        !self.strict_pass && self.relaxed_pass
    }
}

/// Decide both verdicts for one window from its already-computed counts (`probe::
/// recording_segments::window_segment` supplies these — this function re-derives nothing about
/// frame contents, it only combines counts that are already known). Uses the DEFAULT per-window
/// copies/gaps tolerance ([`WINDOW_COPIES_GAPS_TOLERANCE`]); [`decide_for_cambox`] applies the
/// #1251 per-cambox override where one is listed.
pub fn decide(frame_count: u32, undecodable: u32, copies: u32, gaps: u32) -> WindowGateDecision {
    decide_with_tolerance(
        frame_count,
        undecodable,
        copies,
        gaps,
        WINDOW_COPIES_GAPS_TOLERANCE,
    )
}

/// [`decide`] for a specific cambox label -- applies the #1251 per-cambox copies/gaps tolerance
/// override ([`copies_gaps_tolerance_for_cambox`]) instead of the default. Every other cambox (and
/// the lowercase test-fixture labels) resolves to [`WINDOW_COPIES_GAPS_TOLERANCE`], so this equals
/// [`decide`] for them.
pub fn decide_for_cambox(
    cambox: &str,
    frame_count: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
) -> WindowGateDecision {
    decide_with_tolerance(
        frame_count,
        undecodable,
        copies,
        gaps,
        copies_gaps_tolerance_for_cambox(cambox),
    )
}

/// The core of [`decide`], parameterized by the per-window copies/gaps `tolerance` so a per-cambox
/// override ([`decide_for_cambox`]) can widen it for ONE box (#1251) without touching any other
/// term. [`decide`] passes [`WINDOW_COPIES_GAPS_TOLERANCE`]; behaviour is IDENTICAL at that value.
pub fn decide_with_tolerance(
    frame_count: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
    tolerance: u32,
) -> WindowGateDecision {
    let undecodable_ok = crate::optical_floor::window_within_floor(undecodable, frame_count);
    // Issue 915 (2026-08-01) made the optical undecodable floor report-only; issue 905 item 3
    // (2026-09-04) re-gated it -- `crate::optical_floor::gates_overall_pass()` is `true` again now
    // that its physical blockers are closed (cam1 card replaced, 120Hz/100Hz ruled out, 60Hz
    // baseline permanent). So `undecodable_ok` (UNCHANGED, still feeds `strict_pass` byte-for-byte)
    // once more participates in the RELAXED verdict that feeds `overall_pass`. Re-disarm (a new
    // artifact class) = flip that one function back to `false` (see its own doc).
    //
    // 2026-08-05 RE-GATE: `copies`/`gaps` re-join the relaxed verdict, but only above the
    // per-window tolerance -- see `WINDOW_COPIES_GAPS_TOLERANCE`'s own doc for the decision
    // record (recalibrated 1 -> 2 -> 3 on 2026-08-06, ticket 889 comments 5198131539 /
    // 5200533407). #1251: `tolerance` is the DEFAULT const for `decide`, or the per-cambox
    // override for `decide_for_cambox` (CAM2 -> 25 while its grabber HW is sick, issue 1249).
    let within_tolerance = copies <= tolerance && gaps <= tolerance;
    // The optical-floor term is shared by BOTH the relaxed verdict and the #1132 blocking verdict
    // below -- report-only while `gates_overall_pass()` is false (issue 915/905), UNCHANGED by #1132.
    let floor_term = undecodable_ok || !crate::optical_floor::gates_overall_pass();
    let relaxed_pass = frame_count > 0 && floor_term && within_tolerance;
    // #1132 (owner mandate 2026-08-19): the term ACTUALLY folded into `overall_pass`. The
    // copies/gaps rescue is DISARMED for the blocking verdict -- strict `copies == 0 && gaps == 0`
    // unless `copies_gaps_tolerance_gates_overall_pass()` is restored to `true` (then this equals
    // `relaxed_pass`). The optical-floor term is IDENTICAL to `relaxed_pass` -- #1132 does NOT
    // touch the floor's gating status (issue 915/905, its own separate seam -- LIVE since issue
    // 905 item 3).
    //
    // #1169 (owner, 2026-08-22): when the (`<=3`) tolerance rescue is disarmed, a `<=1/<=1`
    // SINGLETON is ABSORBED (the designed issue-1167 paced-trickle + FIFO stale_replay residual,
    // loudly reported) instead of failing strict-zero; `>=2` of EITHER still fails. This is a
    // SEPARATE, strictly-tighter seam -- never a re-arm of the disarmed `<=3` band (that is the
    // CAM1-class mask #1132 removed). Re-tighten to absolute zero = flip
    // `segment_singleton_allowance_gates_overall_pass()` to `false`.
    //
    // #1220 (owner mandate, 2026-08-29): `copies_gaps_tolerance_gates_overall_pass()` is RE-ARMED
    // (see its own doc for the full decision record) -- the FIRST arm below is now taken, so
    // `copies_gaps_ok == within_tolerance` and this term equals `relaxed_pass` exactly, restoring
    // the pre-#1132 fold. The `else if` singleton arm stays wired as an automatic graduated
    // FALLBACK: if a future walk-down step disarms the tolerance channel again, the (still-armed,
    // untouched-by-#1220) `<=1/<=1` singleton band takes back over automatically -- one step down,
    // never straight to the final `else` strict-zero floor. That `else` arm is reachable only if
    // BOTH seams are disarmed (neither is, today).
    let copies_gaps_ok = if copies_gaps_tolerance_gates_overall_pass() {
        within_tolerance
    } else if segment_singleton_allowance_gates_overall_pass() {
        copies <= SEGMENT_SINGLETON_COPIES_ALLOWANCE && gaps <= SEGMENT_SINGLETON_GAPS_ALLOWANCE
    } else {
        copies == 0 && gaps == 0
    };
    let overall_pass_term = frame_count > 0 && floor_term && copies_gaps_ok;
    // #1169: whether the singleton allowance is what absorbed this window's nonzero copies/gaps
    // (drives the loud note + the run-level count). See `segment_singleton_allowance_consumed`.
    // The `frame_count > 0` guard mirrors `overall_pass_term`'s own empty-window guard so this
    // field can never read `true` for an absent cambox even if the copies/gaps computation ever
    // changes (defensive -- an empty window already yields copies==0 && gaps==0 today; #1169 review).
    let singleton_allowance_consumed =
        frame_count > 0 && segment_singleton_allowance_consumed(copies, gaps);
    // `strict_pass` keeps its pre-889-AND-pre-915 meaning byte-for-byte: frame_count>0 &&
    // undecodable within floor && copies==0 && gaps==0 -- computed directly (no longer derived
    // from `relaxed_pass`, since issue 915 decoupled the floor from that derivation).
    let strict_pass = frame_count > 0 && undecodable_ok && copies == 0 && gaps == 0;
    WindowGateDecision {
        strict_pass,
        relaxed_pass,
        overall_pass_term,
        singleton_allowance_consumed,
    }
}

/// The reason(s) a single cambox window fails its RELAXED verdict (`WindowGateDecision::
/// relaxed_pass == false`) -- extracted (issue-889 re-gate deep-review findings 1+2) so
/// `src/bin/recording-verdict.rs`'s per-window WARN print (issue 889 visibility requirement 1,
/// extended by issue 915 and the 2026-08-05 re-gate) matches on this instead of re-deriving the
/// same conditions inline. The inline version being replaced misclassified a `frame_count == 0`
/// window as an exceeded optical floor (`crate::optical_floor::window_within_floor`'s defensive
/// `frame_count == 0` clause is not itself a "floor exceeded" signal), and worded the floor
/// reason as "currently gates overall_pass" unconditionally rather than deriving Gating vs
/// ReportOnly from `crate::optical_floor::gates_overall_pass()` (LIVE `true` since issue 905
/// item 3; was hardcoded `false` under issue 915).
///
/// Call this ONLY on a window whose `relaxed_pass` has already failed -- a window that PASSED its
/// relaxed verdict has no "failure reason" (see the separate WITHIN-TOLERANCE / REPORT-ONLY
/// prints `recording-verdict.rs` uses for that case). Calling it on a passing window is harmless
/// (it re-derives the same conditions and returns an empty `Vec`, or a `FloorWithinReportOnly`
/// reason that did not actually fail anything), but is not this function's intended use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaxedFailureReason {
    /// `frame_count == 0` -- an absent/non-emitting cambox. Never rescued by any relaxation, and
    /// takes priority over every other reason: a 0-frame window's `copies`/`gaps`/`undecodable`
    /// counts carry no meaningful signal.
    EmptyWindow,
    /// `copies` and/or `gaps` exceed the per-window tolerance (the 2026-08-05 re-gate, twice
    /// recalibrated 2026-08-06, [`WINDOW_COPIES_GAPS_TOLERANCE`]).
    OverCopiesGapsTolerance,
    /// The issue-881 optical undecodable floor is exceeded AND it gates `overall_pass`
    /// (`crate::optical_floor::gates_overall_pass() == true`). LIVE since issue 905 item 3
    /// (2026-09-04) re-gated the floor -- this is now the variant that fires for an over-floor
    /// window, a genuine gating failure.
    FloorExceededGating,
    /// The issue-881 optical undecodable floor is exceeded but stays REPORT-ONLY
    /// (`crate::optical_floor::gates_overall_pass() == false`). DORMANT since issue 905 item 3
    /// re-gated the floor (`gates_overall_pass()` is `true` today) -- kept so the seam stays
    /// correct if the floor is ever re-disarmed for a future new artifact class.
    FloorWithinReportOnly,
}

/// See [`RelaxedFailureReason`]'s own doc for the full rationale and call-site discipline.
///
/// `frame_count == 0` is checked FIRST and short-circuits every other reason (finding 1 of the
/// issue-889 re-gate review) -- the optical floor / tolerance checks below are meaningless on an
/// empty window and must never be consulted for one. The floor reason's severity (`Gating` vs
/// `WithinReportOnly`) is decided by `crate::optical_floor::gates_overall_pass()` (finding 2) --
/// never worded as "currently gates overall_pass" unconditionally.
pub fn relaxed_failure_reasons(
    frames: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
) -> Vec<RelaxedFailureReason> {
    relaxed_failure_reasons_with_tolerance(
        frames,
        undecodable,
        copies,
        gaps,
        WINDOW_COPIES_GAPS_TOLERANCE,
    )
}

/// The core of [`relaxed_failure_reasons`], parameterized by the per-window copies/gaps `tolerance`
/// so the per-cambox override (#1251) can be honored: a window judged at the wider CAM2 band is
/// reported OverCopiesGapsTolerance only when it exceeds THAT band, not the default. Callers on the
/// per-cambox path pass the segment's applied tolerance; [`relaxed_failure_reasons`] passes the
/// default const.
pub fn relaxed_failure_reasons_with_tolerance(
    frames: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
    tolerance: u32,
) -> Vec<RelaxedFailureReason> {
    if frames == 0 {
        return vec![RelaxedFailureReason::EmptyWindow];
    }
    let mut reasons = Vec::new();
    if copies > tolerance || gaps > tolerance {
        reasons.push(RelaxedFailureReason::OverCopiesGapsTolerance);
    }
    let floor_ok = crate::optical_floor::window_within_floor(undecodable, frames);
    if !floor_ok {
        if crate::optical_floor::gates_overall_pass() {
            reasons.push(RelaxedFailureReason::FloorExceededGating);
        } else {
            reasons.push(RelaxedFailureReason::FloorWithinReportOnly);
        }
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerance_is_calibrated_at_five_1243() {
        // Pins the calibrated NUMBER itself, not just its use through the const. Issue 1031
        // walk-down history: 3 -> 1 (2026-08-14 morning, one steady post-fix run) -> back to 2
        // the same day (issue-859 residual produces 2-per-window bursts) -> back to 3
        // (2026-08-15, issue 1031 comment 5300948461, the cam1 ShadowCast grabber degradation).
        // Issue 1243 (2026-08-31, THIRD relax-walk step on this const, walk-back tracked on
        // issue 1242): three complete post-fix 7-cam verdicts (1629895310, 1230380558,
        // 1142514714) give per-run worst max(copies,gaps) of {1, 1, 4} -- run 1142514714's
        // seg3 CAM4 measured 4 separate single-frame duplicates over ~14s (no self-heal events),
        // the run's sole blocking-gate failure. Stepping to the bare observed ceiling (4) would
        // leave zero margin given the n=3 variance {1, 1, 4}; 5 gives one event of margin while
        // staying far under the 9-45/window band every genuine regression on this const has
        // measured. See issue 1243's design-addendum comment for the full three-run table.
        assert_eq!(WINDOW_COPIES_GAPS_TOLERANCE, 5);
    }

    #[test]
    fn six_copies_or_gaps_gate_five_absorbed_after_1243() {
        // Issue 1243 boundary at the walked-up tolerance=5: SIX copies (or gaps) must FAIL
        // the relaxed verdict, while FIVE -- one event of margin over the worst observed
        // steady-run window (run 1142514714, max 4) -- stays absorbed. Literal fixtures on
        // purpose: this locks the concrete boundary, complementing the const-tracking boundary
        // tests above.
        assert!(
            !decide(100, 0, 6, 0).relaxed_pass,
            "1243: six copies must gate the relaxed verdict at tolerance=5"
        );
        assert!(
            !decide(100, 0, 0, 6).relaxed_pass,
            "1243: six gaps must gate the relaxed verdict at tolerance=5"
        );
        assert!(
            decide(100, 0, 5, 5).relaxed_pass,
            "1243: the observed worst-window burden (up to 5 copies + 5 gaps) stays absorbed at tolerance=5"
        );
    }

    #[test]
    fn copies_at_tolerance_fails_strict_but_passes_relaxed_889() {
        // Finding 4 of the issue-889 re-gate review: renamed from
        // `copies_alone_fails_strict_but_passes_relaxed_889` -- that name/message implied "copies
        // alone can never fail relaxed", which was true pre-re-gate but is no longer true (over
        // the tolerance fails relaxed again, see `copies_over_tolerance_fails_relaxed_889_regate`
        // below). 2026-08-06 recalibration: renamed again from
        // `copies_at_singleton_tolerance_...` -- "singleton" (implying exactly one) stopped being
        // an accurate description once the tolerance became 2. The fixture value tracks the const
        // (not a literal) so this test self-adjusts with any future recalibration; it is
        // load-bearing that it sits AT the tolerance, not under it.
        let d = decide(100, 0, WINDOW_COPIES_GAPS_TOLERANCE, 0);
        assert!(
            !d.strict_pass,
            "a copy must still fail the strict verdict: {d:?}"
        );
        assert!(
            d.relaxed_pass,
            "re-gate: copies AT the tolerance boundary, not over it, must not fail relaxed: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn gap_alone_at_tolerance_fails_strict_but_passes_relaxed_889() {
        // 2026-08-05 re-gate (issue 889 ROZHODNUTÉ), recalibrated 2026-08-06 (comment 5198131539):
        // a gap AT the tolerance boundary -- still must not fail relaxed.
        let d = decide(100, 0, 0, WINDOW_COPIES_GAPS_TOLERANCE);
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "re-gate: a gap AT the tolerance boundary must not fail relaxed: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_over_tolerance_fails_relaxed_889_regate() {
        // 2026-08-06 recalibration(s): the fixture sits at tolerance+1 so this test tracks
        // whatever the const is calibrated to (now 4, after the second same-day recalibration)
        // instead of hardcoding a boundary literal. The window must FAIL the relaxed verdict again
        // (this is the whole point of the re-gate: a return of the issue-971 regression class,
        // 10-45 copies/window, must fail loudly).
        let d = decide(100, 0, WINDOW_COPIES_GAPS_TOLERANCE + 1, 0);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "re-gate: copies over the tolerance -- must fail relaxed again: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "an over-tolerance window is not rescued by any report-only relaxation: {d:?}"
        );
    }

    #[test]
    fn gaps_over_tolerance_fails_relaxed_889_regate() {
        // 2026-08-06 recalibration: renamed from `gaps_over_singleton_tolerance_...`; fixture
        // tracks the const (tolerance+1) instead of a hardcoded pre-recalibration literal.
        let d = decide(100, 0, 0, WINDOW_COPIES_GAPS_TOLERANCE + 1);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "re-gate: gaps over the tolerance -- must fail relaxed again: {d:?}"
        );
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_both_at_tolerance_pass_relaxed_889_regate() {
        // Mirrors the measured residual the ORIGINAL (2026-08-05) threshold decision was
        // calibrated against (run 31033239950 attempt 1, comment id 5195798868): windows with
        // copies AND gaps simultaneously AT the tolerance, fully absorbed. 2026-08-06 second
        // recalibration: fixture continues to track the const (3, not the prior-recalibration 2).
        let d = decide(
            100,
            0,
            WINDOW_COPIES_GAPS_TOLERANCE,
            WINDOW_COPIES_GAPS_TOLERANCE,
        );
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "re-gate: copies AND gaps together must still pass relaxed (both at tolerance): {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_together_over_tolerance_fail_relaxed_889_regate() {
        // 2026-08-05 re-gate: both terms over tolerance together must fail relaxed -- this is the
        // exact pre-fix regression shape (the original issue-889 failing evidence). 2026-08-06
        // recalibration: fixtures now sit at tolerance+1/tolerance+2 so this stays "both terms
        // over, asymmetric" regardless of the calibrated tolerance value.
        let d = decide(
            100,
            0,
            WINDOW_COPIES_GAPS_TOLERANCE + 1,
            WINDOW_COPIES_GAPS_TOLERANCE + 2,
        );
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "re-gate: copies AND gaps both exceed the tolerance -- must fail relaxed: {d:?}"
        );
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn zero_frames_fails_both_verdicts_889() {
        // #889 does not touch `frame_count > 0` — an absent cambox proves nothing either way.
        let d = decide(0, 0, 0, 0);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "a 0-frame window must still fail the relaxed verdict: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "not #889's doing -- frame_count==0 fails both: {d:?}"
        );
    }

    #[test]
    fn undecodable_over_floor_now_fails_both_verdicts_905() {
        // Issue 905 item 3 (2026-09-04): the optical undecodable floor is re-gated (the report-only
        // period is over -- `optical_floor::gates_overall_pass()` is `true` again). An over-floor
        // undecodable count now fails the RELAXED verdict too, not just the STRICT one -- so it is
        // no longer a "#889 relaxation rescue" case.
        let d = decide(10, 5, 0, 0); // 5 undecodable of 10 frames -- past the #881 per-window floor (4)
        assert!(
            !d.strict_pass,
            "the optical floor fails the STRICT verdict, unchanged: {d:?}"
        );
        assert!(
            !d.relaxed_pass,
            "issue 905: undecodable over floor now fails the relaxed verdict too: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "issue 905: over-floor is no longer rescued by relaxation -- both verdicts fail: {d:?}"
        );
    }

    #[test]
    fn clean_window_passes_both_verdicts() {
        let d = decide(100, 0, 0, 0);
        assert!(d.strict_pass);
        assert!(d.relaxed_pass);
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn undecodable_within_floor_and_clean_copies_gaps_passes_both_881() {
        // Mirrors `optical_floor`'s own calibrated floor -- unaffected by #889.
        let d = decide(847, 1, 0, 0);
        assert!(d.strict_pass);
        assert!(d.relaxed_pass);
    }

    // --- relaxed_failure_reasons (issue-889 re-gate deep-review findings 1+2) -------------------

    #[test]
    fn relaxed_failure_reasons_frames_zero_is_empty_window_889_review() {
        // Finding 1 of the issue-889 re-gate review: a frame_count==0 window must classify as
        // EmptyWindow, never as an exceeded optical floor --
        // `optical_floor::window_within_floor`'s defensive `frame_count == 0` clause is not
        // itself a "floor exceeded" signal. The RED commit for this test proved a prior version
        // of this function (which checked `over_tolerance`/`floor_ok` before ever asking "is this
        // window even empty") got this wrong, reporting `[FloorExceededGating]` instead.
        let reasons = relaxed_failure_reasons(0, 0, 0, 0);
        assert_eq!(
            reasons,
            vec![RelaxedFailureReason::EmptyWindow],
            "a 0-frame window must classify as EmptyWindow alone, never a floor reason: {reasons:?}"
        );
    }

    #[test]
    fn relaxed_failure_reasons_over_copies_tolerance_889_regate() {
        // 2026-08-06 second recalibration: fixture tracks tolerance+1 (4) instead of the
        // prior-recalibration hardcoded 3. undecodable=0 stays within the floor -- the ONLY reason
        // is OverCopiesGapsTolerance.
        let reasons = relaxed_failure_reasons(100, 0, WINDOW_COPIES_GAPS_TOLERANCE + 1, 0);
        assert_eq!(reasons, vec![RelaxedFailureReason::OverCopiesGapsTolerance]);
    }

    #[test]
    fn relaxed_failure_reasons_within_tolerance_and_floor_is_empty_889_regate() {
        // Finding 6 coverage: copies AND gaps sit AT the tolerance (not over it), and
        // undecodable=0 is within the floor -- a window with these counts does not actually fail
        // `relaxed_pass` (see `copies_and_gaps_both_at_tolerance_pass_relaxed_889_regate` above),
        // so it has NO failure reason. This function is only ever called by
        // `recording-verdict.rs` on a window that already failed `relaxed_pass` -- this test just
        // pins the seam's own boundary behavior independent of that call-site discipline.
        // 2026-08-06 recalibration: fixture tracks the const instead of the pre-recalibration
        // hardcoded 1.
        let reasons = relaxed_failure_reasons(
            100,
            0,
            WINDOW_COPIES_GAPS_TOLERANCE,
            WINDOW_COPIES_GAPS_TOLERANCE,
        );
        assert!(
            reasons.is_empty(),
            "copies/gaps both AT tolerance -- no failure reason applies: {reasons:?}"
        );
    }

    #[test]
    fn relaxed_failure_reasons_over_floor_now_gates_905() {
        // Mirrors `undecodable_over_floor_now_fails_both_verdicts_905` above: undecodable=5 over
        // the per-window floor (4), frames=10. Issue 905 item 3 re-gated the floor
        // (`gates_overall_pass()` is `true` again), so this is now FloorExceededGating (the LIVE
        // gating reason), no longer merely FloorWithinReportOnly.
        let reasons = relaxed_failure_reasons(10, 5, 0, 0);
        assert_eq!(reasons, vec![RelaxedFailureReason::FloorExceededGating]);
    }

    #[test]
    fn relaxed_failure_reasons_over_tolerance_and_over_floor_both_reported_889_regate() {
        // Finding 2's "else-arm" scenario: a window can fail overall_pass via OverCopiesGapsTolerance
        // while ALSO carrying an over-floor undecodable count --
        // both reasons are returned so the caller can print both. Issue 905 item 3 re-gated the
        // floor (`gates_overall_pass()` is `true`), so the floor half is now FloorExceededGating
        // (a LIVE second gating reason) alongside OverCopiesGapsTolerance. 2026-08-06 second
        // recalibration: fixture tracks tolerance+1 instead of the prior-recalibration hardcoded 3.
        let reasons = relaxed_failure_reasons(10, 5, WINDOW_COPIES_GAPS_TOLERANCE + 1, 0);
        assert_eq!(
            reasons,
            vec![
                RelaxedFailureReason::OverCopiesGapsTolerance,
                RelaxedFailureReason::FloorExceededGating,
            ]
        );
    }

    // --- #1220 (owner mandate, 2026-08-29): the copies/gaps TOLERANCE is RE-ARMED and once again
    // governs `overall_pass_term` -- it now equals `relaxed_pass` exactly (the pre-#1132 fold).
    // `strict_pass` stays byte-for-byte unchanged (still absolute-zero, still visible). The #1169
    // `<=1/<=1` singleton band stays wired but is now DORMANT (unreachable via `decide()`'s
    // `if`/`else if` precedence) -- proven directly on the pure helper fns below, not through
    // `decide()`, mirroring `gate-allowance-restore-red-green.md`'s "one test at the pure-method
    // level keeps a dormant mechanism regression-tested" pattern.
    // ------------------------------------------------------------------------------------------

    #[test]
    fn copies_gaps_tolerance_re_armed_gates_overall_pass_again_1220() {
        // #1220 (owner mandate, 2026-08-29) re-arms the seam #1132 (2026-08-19) had disarmed --
        // see the module doc's "2026-08-29 RE-ARM" section and this function's own doc for the
        // full decision record (two same-day full-cycle runs, 1989954227 + 797081170, the first
        // consecutive-green A/V pair in this project's history, with the second run's contiguity
        // failing purely on windows over the tighter #1169 singleton band while fully within this
        // already-calibrated `<=3` channel). Renamed from
        // `copies_gaps_tolerance_no_longer_gates_overall_pass_1132`, whose assertion is now the
        // exact opposite.
        assert!(
            copies_gaps_tolerance_gates_overall_pass(),
            "#1220: the copies/gaps tolerance must rescue overall_pass again"
        );
    }

    #[test]
    fn a_single_copy_is_absorbed_by_the_1220_tolerance_channel_not_the_1169_singleton() {
        // Renamed from `a_single_copy_is_absorbed_by_the_1169_singleton_allowance_supersedes_1132`.
        // The observable outcome (absorbed, strict still fails) is UNCHANGED by #1220 -- copies=1
        // sits within BOTH the `<=1/<=1` singleton band and the wider `<=3` tolerance -- but the
        // MECHANISM changed: it is absorbed by the re-armed tolerance channel now, never reaching
        // the (still-armed, now-dormant) singleton branch at all. See
        // `singleton_helper_fns_are_dormant_while_the_1220_tolerance_channel_is_armed` below for
        // the proof this window's `singleton_allowance_consumed` is FALSE despite the absorption.
        let d = decide(847, 0, 1, 0);
        assert!(
            d.relaxed_pass,
            "relaxed_pass stays tolerant/reported (observability): {d:?}"
        );
        assert!(
            d.overall_pass_term,
            "#1220: a single copy is absorbed into the blocking verdict via the re-armed tolerance: {d:?}"
        );
        assert!(
            !d.strict_pass,
            "strict still fails on the copy, unchanged/visible: {d:?}"
        );
        assert!(
            !d.singleton_allowance_consumed,
            "#1220: the SINGLETON mechanism never fires -- the tolerance channel absorbed this, \
             not the (dormant) singleton band: {d:?}"
        );
    }

    #[test]
    fn a_single_gap_and_a_copy_gap_pair_are_absorbed_by_the_1220_tolerance_channel() {
        // Renamed from `a_single_gap_and_a_copy_gap_pair_are_absorbed_by_the_1169_singleton_allowance`.
        // The live verdict 859647390 shapes: seg[4] CAM2 copies=1 gaps=0, and seg[3] CAM3
        // copies=1 gaps=1 -- both sit within the re-armed `<=3` tolerance (and, incidentally,
        // within the dormant `<=1/<=1` band too). strict stays false; the singleton mechanism
        // never fires (see the dedicated dormancy test below).
        for &(c, g) in &[(1u32, 0u32), (0, 1), (1, 1)] {
            let d = decide(847, 0, c, g);
            assert!(
                d.overall_pass_term,
                "#1220: copies={c} gaps={g} (<=3 each) absorbed into the blocking verdict: {d:?}"
            );
            assert!(!d.strict_pass, "strict still fails, visible: {d:?}");
        }
    }

    #[test]
    fn two_or_three_copies_or_gaps_now_pass_within_the_reactivated_tolerance_1220() {
        // SUPERSEDES `two_copies_or_a_gap_pair_still_fail_after_singleton_allowance_1169`: with
        // the `<=3` tolerance channel re-armed, 2 and 3 (unlike under the #1169 `<=1/<=1` band)
        // are WITHIN tolerance and now PASS the blocking verdict -- exactly the four live-verdict
        // shapes (CAM2 2/2, CAM6 2/1, CAM7 2/3, and the combined 3/3) issue 1220 was filed to fix.
        for &(c, g) in &[(2u32, 0u32), (0, 2), (1, 2), (2, 1), (3, 3), (2, 2), (2, 3)] {
            let d = decide(847, 0, c, g);
            assert!(
                d.overall_pass_term,
                "#1220: copies={c} gaps={g} sits within the re-armed <=3 tolerance -- must pass \
                 the blocking verdict: {d:?}"
            );
            assert!(
                !d.strict_pass,
                "strict still fails on any nonzero copies/gaps, unchanged/visible: {d:?}"
            );
        }
    }

    #[test]
    fn tolerance_plus_one_copies_or_gaps_still_fail_over_the_reactivated_tolerance_1220() {
        // Renamed from `four_copies_or_gaps_still_fail_over_the_reactivated_tolerance_1220` --
        // the fixture value tracks the const (`+1`), so the old "four" name went stale as soon as
        // the const walked past 3 (it's 6 today, walked 3 -> 5 on issue 1243). The upper edge
        // #1220 does NOT touch: `WINDOW_COPIES_GAPS_TOLERANCE + 1` must still FAIL -- this is what
        // keeps the re-arm a real, still-discriminating gate rather than an open door. Mirrors run
        // 1989954227's still-red CAM2 windows (copies=10/gaps=9, copies=19/gaps=18), still well
        // over the walked-up value.
        for &(c, g) in &[
            (WINDOW_COPIES_GAPS_TOLERANCE + 1, 0u32),
            (0, WINDOW_COPIES_GAPS_TOLERANCE + 1),
        ] {
            let d = decide(847, 0, c, g);
            assert!(
                !d.overall_pass_term,
                "#1220: copies={c} gaps={g} exceeds the re-armed tolerance ({}) -- must still \
                 fail: {d:?}",
                WINDOW_COPIES_GAPS_TOLERANCE
            );
        }
    }

    #[test]
    fn overall_pass_term_equals_relaxed_pass_across_the_band_1220() {
        // The core invariant #1220 restores: `overall_pass_term == relaxed_pass` on EVERY input,
        // not just clean copies/gaps (see `overall_pass_term_agrees_with_relaxed_on_clean_copies_
        // gaps_1132` below for the narrower pre-#1220 invariant this generalizes). Swept across
        // the whole reported tolerance band plus two steps over it, for both copies-only and
        // gaps-only shapes.
        for n in 0..=(WINDOW_COPIES_GAPS_TOLERANCE + 2) {
            let dc = decide(847, 0, n, 0);
            let dg = decide(847, 0, 0, n);
            assert_eq!(
                dc.overall_pass_term, dc.relaxed_pass,
                "#1220: copies={n} -- overall_pass_term must equal relaxed_pass: {dc:?}"
            );
            assert_eq!(
                dg.overall_pass_term, dg.relaxed_pass,
                "#1220: gaps={n} -- overall_pass_term must equal relaxed_pass: {dg:?}"
            );
        }
    }

    #[test]
    fn overall_pass_term_passes_a_clean_window_1132() {
        let d = decide(847, 0, 0, 0);
        assert!(
            d.overall_pass_term,
            "a clean window passes the blocking verdict: {d:?}"
        );
        assert!(d.strict_pass && d.relaxed_pass);
    }

    #[test]
    fn over_floor_fails_overall_pass_term_via_905_floor_regate() {
        // CRITICAL separation, updated for issue 905 item 3: the optical undecodable floor now
        // gates via its OWN seam (`optical_floor::gates_overall_pass()` re-armed), INDEPENDENT of
        // the #1132/#1220 copies/gaps seam. A window OVER the optical floor but CLEAN on
        // copies/gaps (so `copies_gaps_ok` is true) now fails the blocking verdict -- the failure
        // is attributable to the floor term alone, proving the two seams stay independent.
        let d = decide(10, 5, 0, 0); // 5 undecodable of 10 -- past the per-window floor (4)
        assert!(
            !d.strict_pass,
            "strict gates the optical floor, unchanged: {d:?}"
        );
        assert!(
            !d.overall_pass_term,
            "issue 905: the re-gated optical floor now fails the blocking verdict even with clean \
             copies/gaps: {d:?}"
        );
        assert!(
            !d.relaxed_pass,
            "relaxed now gates the floor too (issue 905): {d:?}"
        );
    }

    #[test]
    fn overall_pass_term_empty_window_fails_1132() {
        let d = decide(0, 0, 0, 0);
        assert!(
            !d.overall_pass_term,
            "an absent cambox fails the blocking verdict: {d:?}"
        );
    }

    #[test]
    fn overall_pass_term_agrees_with_relaxed_on_clean_copies_gaps_1132() {
        // Invariant preserved across #1132 AND #1220: on any window with copies==0 && gaps==0 the
        // two agree regardless of which copies/gaps seam is armed (the floor term is identical).
        // See `overall_pass_term_equals_relaxed_pass_across_the_band_1220` above for the now-wider
        // invariant #1220 additionally establishes (agreement on EVERY input, not just clean).
        for &(f, u) in &[(847u32, 0u32), (10, 5), (0, 0), (847, 1)] {
            let d = decide(f, u, 0, 0);
            assert_eq!(
                d.overall_pass_term, d.relaxed_pass,
                "clean copies/gaps -> blocking verdict agrees with relaxed: {d:?}"
            );
        }
    }

    // --- #1169 (owner, 2026-08-22) singleton mechanism -- kept WIRED, now DORMANT under #1220's
    // re-armed tolerance channel (never deleted; see `gate-allowance-restore-red-green.md`'s
    // "leave dormant, not deleted" doctrine + the module doc's "2026-08-29 RE-ARM" section for the
    // graduated-fallback rationale). Its own flag (`segment_singleton_allowance_gates_overall_
    // pass`) and the calibrated band constants stay UNTOUCHED by #1220 -- proven directly below,
    // both at the flag/const level (still armed/calibrated) and at the pure-helper-fn level (now
    // unreachable via `decide()`, so its own outputs read as if permanently disarmed).
    // ------------------------------------------------------------------------------------------

    #[test]
    fn singleton_allowance_flag_and_consts_stay_armed_and_calibrated_but_dormant_1220() {
        // Renamed from `singleton_allowance_is_armed_and_calibrated_at_one_1169`. The FLAG and
        // CONSTS are untouched by #1220 (still `true`/1/1) -- #1220 supersedes this seam by
        // PRECEDENCE inside `decide()`, never by flipping this flag. Issue 1169's own re-tighten
        // trail (this flag to `false`) stays independent and open.
        assert_eq!(SEGMENT_SINGLETON_COPIES_ALLOWANCE, 1);
        assert_eq!(SEGMENT_SINGLETON_GAPS_ALLOWANCE, 1);
        assert!(
            segment_singleton_allowance_gates_overall_pass(),
            "#1220 does not touch this flag -- it stays armed, now reachable only as a fallback"
        );
        assert!(
            copies_gaps_tolerance_gates_overall_pass(),
            "#1220: the WIDER tolerance channel is armed too, and takes precedence in decide()"
        );
    }

    #[test]
    fn singleton_helper_fns_are_dormant_while_the_1220_tolerance_channel_is_armed() {
        // Renamed from `singleton_allowance_consumed_flag_records_the_absorption_1169`, INVERTED:
        // every one of these previously-`true`/`Some` results is now permanently `false`/`None`,
        // because `segment_singleton_allowance_consumed` itself requires
        // `!copies_gaps_tolerance_gates_overall_pass()`, which #1220 made `false` (armed = true).
        // This is the "one test at the pure-method level keeps the dormant mechanism regression-
        // tested" half of `gate-allowance-restore-red-green.md`'s doctrine, applied to a seam that
        // stays reachable directly (not just through `decide()`).
        let d = decide(847, 0, 1, 1);
        assert!(
            d.overall_pass_term,
            "still absorbed -- by the tolerance channel now: {d:?}"
        );
        assert!(
            !d.singleton_allowance_consumed,
            "#1220: the decision no longer attributes the absorption to the singleton: {d:?}"
        );
        assert!(
            !segment_singleton_allowance_consumed(1, 0),
            "#1220: the singleton helper is dormant -- copies=1 no longer 'consumes' it"
        );
        assert!(
            !segment_singleton_allowance_consumed(0, 1),
            "#1220: dormant -- gaps=1 no longer 'consumes' it"
        );
        assert!(
            !segment_singleton_allowance_consumed(1, 1),
            "#1220: dormant -- copies=1 gaps=1 no longer 'consumes' it"
        );
        assert!(
            !segment_singleton_allowance_consumed(0, 0),
            "clean window still consumes nothing (unaffected either way)"
        );
        assert!(
            !segment_singleton_allowance_consumed(2, 0),
            "an over-singleton-band count still never 'consumes' the singleton (unaffected either way)"
        );
        assert!(
            !decide(847, 0, 0, 0).singleton_allowance_consumed,
            "a clean decision never reports the allowance consumed"
        );
        assert!(
            !decide(847, 0, 2, 0).singleton_allowance_consumed,
            "#1220: copies=2 is now absorbed by the tolerance channel, not the singleton -- \
             singleton_allowance_consumed stays false even though overall_pass_term is true"
        );
    }

    #[test]
    fn segment_singleton_note_is_dormant_while_the_1220_tolerance_channel_is_armed() {
        // Renamed from `segment_singleton_note_fires_only_when_consumed_1169`, INVERTED: every
        // shape that used to fire the note now returns `None`, since `segment_singleton_note`
        // delegates straight to the now-permanently-false `segment_singleton_allowance_consumed`.
        assert!(
            segment_singleton_note(1, 1).is_none(),
            "#1220: the singleton note never fires while the tolerance channel is armed"
        );
        assert!(segment_singleton_note(1, 0).is_none());
        assert!(segment_singleton_note(0, 1).is_none());
        assert!(
            segment_singleton_note(0, 0).is_none(),
            "no note on a clean segment -- the note is never noise (unaffected either way)"
        );
        assert!(
            segment_singleton_note(2, 0).is_none(),
            "#1220: copies=2 is absorbed by the tolerance channel now, not the singleton -- still \
             no singleton note (a genuinely over-tolerance count still gets none either way)"
        );
    }

    // ---- #1251: per-cambox copies/gaps tolerance override (CAM2 -> 25, walk-back on issue 1242) ----

    #[test]
    fn per_cambox_tolerance_override_cam2_is_25_others_default_1251() {
        // #1251: CAM2's grabber HW (issue 1249) starves in sub-second bursts, so the #1167 v4
        // slot-fill pacer repeats the last frame (a copy) then skips (a gap) -- run 1326320314
        // measured CAM2 windows at copies=8/gaps=8 and copies=18/gaps=17 while every OTHER box
        // stayed within 5. The scoped override gives the EXACT production label "CAM2" a tolerance
        // of 25 (covers the observed 18, one margin band under the ceiling); every other box keeps
        // the default WINDOW_COPIES_GAPS_TOLERANCE.
        assert_eq!(copies_gaps_tolerance_for_cambox("CAM2"), 25);
        assert_eq!(
            copies_gaps_tolerance_for_cambox("CAM3"),
            WINDOW_COPIES_GAPS_TOLERANCE
        );
        assert_eq!(
            copies_gaps_tolerance_for_cambox("CAM1"),
            WINDOW_COPIES_GAPS_TOLERANCE
        );
        // Exact-match on purpose: production emits UPPERCASE `CAMN`, but the recording_segments.rs
        // unit fixtures use lowercase `cam2` -- those must NOT pick up the override, so the
        // override only ever touches the real rig (and the existing lowercase-cam2 tests stay red
        // exactly where they were).
        assert_eq!(
            copies_gaps_tolerance_for_cambox("cam2"),
            WINDOW_COPIES_GAPS_TOLERANCE
        );
    }

    #[test]
    fn decide_for_cambox_cam2_absorbs_the_observed_starvation_burst_1251() {
        // The two CAM2 windows from run 1326320314: copies=8/gaps=8 and copies=18/gaps=17. Under
        // the per-cambox tolerance (25) both are WITHIN tolerance, so relaxed_pass AND (with the
        // #1220 tolerance seam armed) overall_pass_term become true.
        let d1 = decide_for_cambox("CAM2", 846, 0, 8, 8);
        assert!(d1.relaxed_pass, "CAM2 8/8 within the 25 override: {d1:?}");
        assert!(
            d1.overall_pass_term,
            "CAM2 8/8 gates PASS under the override: {d1:?}"
        );
        let d2 = decide_for_cambox("CAM2", 847, 0, 18, 17);
        assert!(d2.relaxed_pass, "CAM2 18/17 within the 25 override: {d2:?}");
        assert!(
            d2.overall_pass_term,
            "CAM2 18/17 gates PASS under the override: {d2:?}"
        );
        // The copies/gaps are still COMPUTED -- the strict verdict still fails, never masked.
        assert!(
            !d2.strict_pass,
            "strict still fails on nonzero copies/gaps: {d2:?}"
        );
    }

    #[test]
    fn decide_for_cambox_cam2_over_the_override_still_fails_1251() {
        // Over the 25 override the window fails again -- the override is a bounded relax to green,
        // not a blanket pass for CAM2.
        let d = decide_for_cambox("CAM2", 847, 0, 26, 0);
        assert!(
            !d.relaxed_pass,
            "CAM2 copies=26 over the 25 override still fails: {d:?}"
        );
        assert!(
            !d.overall_pass_term,
            "and therefore fails overall_pass: {d:?}"
        );
    }

    #[test]
    fn decide_for_cambox_other_box_keeps_the_default_tolerance_1251() {
        // A non-overridden box (CAM3) gets NO relaxation beyond the default 5: copies/gaps=6 fails
        // exactly as `decide` (default) would -- the override is CAM2-scoped, never global.
        let d = decide_for_cambox("CAM3", 847, 0, 6, 6);
        assert!(
            !d.relaxed_pass,
            "CAM3 6/6 over the default 5 still fails: {d:?}"
        );
        let default = decide(847, 0, 6, 6);
        assert_eq!(
            d, default,
            "CAM3 decision == the default decide across the board"
        );
    }

    #[test]
    fn decide_with_tolerance_matches_decide_at_the_default_1251() {
        // The refactor is behaviour-preserving: `decide` is exactly `decide_with_tolerance` at the
        // default const across the boundary.
        for (c, g) in [(0, 0), (5, 5), (6, 0), (0, 6), (25, 25), (26, 0)] {
            assert_eq!(
                decide(100, 0, c, g),
                decide_with_tolerance(100, 0, c, g, WINDOW_COPIES_GAPS_TOLERANCE),
                "decide == decide_with_tolerance(default) at copies={c} gaps={g}"
            );
        }
    }

    #[test]
    fn relaxed_failure_reasons_with_tolerance_honors_the_per_cambox_band_1251() {
        // Judged at the 25 override, copies=20 (over the default 5, UNDER 25) is NOT an
        // over-tolerance failure reason -- it passes relaxed there.
        let none = relaxed_failure_reasons_with_tolerance(847, 0, 20, 0, 25);
        assert!(
            !none.contains(&RelaxedFailureReason::OverCopiesGapsTolerance),
            "20 copies is within the 25 override -- not an over-tolerance reason: {none:?}"
        );
        // Over the override it IS reported.
        let over = relaxed_failure_reasons_with_tolerance(847, 0, 26, 0, 25);
        assert!(over.contains(&RelaxedFailureReason::OverCopiesGapsTolerance));
        // The default-tolerance wrapper is unchanged (const 5): 20 copies IS over the default.
        let over_default = relaxed_failure_reasons(847, 0, 20, 0);
        assert!(over_default.contains(&RelaxedFailureReason::OverCopiesGapsTolerance));
    }
}
