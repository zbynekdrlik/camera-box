//! #401 — phase-locked release cadence in the C ts-align genlock release (vendored libobs).
//!
//! Root cause (run 7020001, 2026-07-02): the pre-#401 ts-align release re-derived the
//! presentation deadline from the wall clock EVERY render tick
//! (`present_ts = wall_now - reserve`) and presented the NEWEST due frame, silently erasing
//! the older due ones (`to_drop = due - 1` with NO counter). With render ticks and capture
//! stamps on the same DanteSync 60 Hz grid, a reserve near a multiple of the frame interval
//! puts the deadline ON a stamp: the ±2 ms render-tick slew then flips that frame
//! due/not-due tick-to-tick — alternating HOLD + silent DROP. Measured live (`NDI cam5`):
//! 43.9–57.7 distinct fps of a 60 fps flow, invisible in the audit (8,511 ids lost with
//! zero counter movement).
//!
//! The fix ports `src/probe/genlock.rs` `ReleaseCadence` (v2, cc815e73e) into
//! `vendor/obs-studio/libobs/obs-source.c`: the deadline comes from a LOCKED per-source
//! boundary that advances exactly one interval per presented frame (slew-immune by
//! construction); the wall clock only acquires the lock; and EVERY discarded frame is
//! counted (`genlock_dropped_due`) — never silent again.
//!
//! v2 (live canary of v1, 2026-07-02, strih `NDI cam5`): v1's wall-based drift guard
//! (`present_ts > boundary + 2*interval + interval/4`) EMBEDS the constant stamp→arrival
//! skew (59 ms live at the 3 ms reserve) and relock-stormed — dropped_due 2918 of 4202
//! received (69 %), relocks 1076. v2 replaces it with a QUEUE-DEPTH backlog guard, which is
//! skew-immune where wall−boundary drift is not; the steady path
//! presents the OLDEST matured frame (strict FIFO, transient 2-frame maturation drains
//! losslessly) and a GAP RESYNC re-anchors past upstream-skipped stamps.
//!
//! #859: that queue-depth threshold is no longer a bare constant. Its original value encoded
//! "steady depth is ~1–2 at ANY skew", which holds only for a SHALLOW source; a source pinned
//! deep for A/V alignment (923 ms on the stream box's `NDI 2ME PGM`) sits at ~28 and exceeded it
//! permanently, relocking every tick and shedding paired duplicate/skip frames. The threshold is
//! now the depth each source's OWN configured latency implies plus the unchanged margin — see
//! `src/genlock_backlog.rs` for the Tier-0-tested decision.
//!
//! This is a SOURCE-presence guard (same convention as tests/distroav_genlock_lockdown.rs,
//! tests/obs_updater_disabled.rs): it runs on DEFAULT features (per-PR Linux CI + local
//! Tier-0), reads the vendored C as text, and fails loudly if a future `git subtree pull`
//! (#44) — or any edit — silently reverts the cadence to the per-tick wall-compare release.

use std::path::PathBuf;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn cadence_state_and_counters_declared_on_obs_source() {
    // The per-source cadence state + honest-loss counters must live on obs_source
    // (obs-internal.h), next to the other genlock audit fields, so the 5s audit line can
    // print them and a reconnect/create zeroes them with the rest (bzalloc).
    let internal = squish(&vendor_file(OBS_INTERNAL));
    for field in [
        "uint64_t genlock_locked_next_boundary_ns;",
        "uint64_t genlock_dropped_due;",
        "uint64_t genlock_relocks;",
        "uint64_t genlock_late_holds;",
    ] {
        assert!(
            internal.contains(field),
            "{OBS_INTERNAL}: #401 — cadence field `{field}` missing from obs_source; the \
             phase-locked release cadence (or its honest drop/relock/late-hold audit) \
             reverted. Re-apply (mirror: src/probe/genlock.rs ReleaseCadence)."
        );
    }
}

#[test]
fn release_is_phase_locked_not_per_tick_wall_compare() {
    // The ts-align release block must key steady-state presentation on the LOCKED
    // boundary (genlock_locked_next_boundary_ns), acquired/re-locked from the wall
    // deadline — not re-derive a due/not-due decision from the wall clock every tick
    // (the pre-#401 hold↔silent-drop churn).
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("genlock_locked_next_boundary_ns"),
        "{OBS_SOURCE}: #401 — the phase-locked release cadence (locked boundary \
         genlock_locked_next_boundary_ns) is gone from the ts-align release; the per-tick \
         wall-compare release loses ~16 of 60 distinct fps at grid-aligned reserves. \
         Re-apply (mirror: src/probe/genlock.rs ReleaseCadence::tick)."
    );
    // v2: the v1 WALL-DRIFT relock guard must be GONE. It compared `present_ts` against
    // `boundary + 2*interval + interval/4`, which EMBEDS the constant stamp→arrival skew
    // (59 ms on the live rig) — the 2026-07-02 canary relock-stormed: dropped_due 2918 of
    // 4202 received, relocks 1076. Any wall−boundary drift threshold reintroduces that
    // skew dependence; the backlog guard must be queue-relative (depth), never wall-based.
    assert!(
        !src.contains("2 * interval + interval / 4"),
        "{OBS_SOURCE}: #401 v2 — the v1 wall-drift relock guard (2*interval + interval/4 \
         vs present_ts − boundary) is BACK; it embeds the constant stamp→arrival skew and \
         relock-storms live (canary: dropped_due 2918/4202, relocks 1076). Use the \
         queue-depth guard (GENLOCK_QDEPTH_RELOCK) — mirror: src/probe/genlock.rs \
         ReleaseCadence::tick (v2)."
    );
    // v2: the QUEUE-DEPTH backlog guard must be present — a named constant so the
    // rationale travels with the threshold. The re-lock jumps to the newest due frame
    // counting every jumped frame (visible catch-up, IMAG latency contract kept).
    assert!(
        src.contains("GENLOCK_QDEPTH_RELOCK"),
        "{OBS_SOURCE}: #401 v2 — the queue-depth backlog guard (GENLOCK_QDEPTH_RELOCK) is \
         missing; without it a genuine stall's backlog never re-locks to the live edge. \
         Mirror src/probe/genlock.rs ReleaseCadence::QDEPTH_RELOCK_MARGIN."
    );
    // #859: the threshold must be RELATIVE to the depth each source's own configured latency
    // implies, not a bare constant. The pre-#859 code compared against the bare 6, whose comment
    // assumed "steady depth is ~1-2 at any skew" — true only for a SHALLOW source. A source
    // pinned deep (923 ms on the stream box's 'NDI 2ME PGM', to A/V-align against the mbc's 1 s
    // mastering) has a steady depth of ~28, so the branch fired EVERY tick and shed a frame on
    // every arrival-jitter excursion: measured as +59 duplicate / +57 skipped frames injected
    // into the strih->stream hop. A `git subtree pull` reverting to the bare comparison would
    // silently reintroduce that, so assert the helper is actually WIRED IN, not merely defined.
    assert!(
        src.contains("genlock_backlog_relock_qdepth(source, reserve_ms, interval)"),
        "{OBS_SOURCE}: #859 — the backlog branch no longer calls \
         genlock_backlog_relock_qdepth(source, reserve_ms, interval); it is back to comparing \
         against a bare constant, which a deep-latency source exceeds permanently (steady depth \
         ~28 at 923 ms) and which therefore relocks every tick and sheds paired duplicate/skip \
         frames. Mirror: src/genlock_backlog.rs backlog_relock_threshold (Tier-0 unit-tested) \
         and src/probe/genlock.rs ReleaseCadence::backlog_relock_qdepth."
    );
    // The margin must stay the ORIGINAL 6 — #859 changed what the threshold is RELATIVE TO, it
    // did not widen the tolerance. A bumped margin would be exactly the "widen the threshold to
    // make it pass" move the standing rule forbids.
    assert!(
        src.contains("#define GENLOCK_QDEPTH_RELOCK_MARGIN 6"),
        "{OBS_SOURCE}: #859 — the backlog MARGIN is no longer the original 6. #859 made the \
         threshold latency-RELATIVE; it must never become latency-relative AND widened."
    );
    // The mirror pointer must survive so the next maintainer finds the PROVEN Rust
    // reference (and its three cadence tests) before touching the C.
    assert!(
        src.contains("ReleaseCadence"),
        "{OBS_SOURCE}: #401 — the mirror pointer to src/probe/genlock.rs ReleaseCadence is \
         gone; the C and Rust implementations will drift apart silently. Re-apply."
    );
}

#[test]
fn every_discarded_frame_is_counted_never_silent() {
    // THE #401 bug: `to_drop = due - 1; while (to_drop--) da_erase(...)` erased stale due
    // frames with NO counter — 8,511 ids lost in run 7020001 with zero audit movement.
    // Every erase on the release path must now count into genlock_dropped_due.
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("source->genlock_dropped_due++"),
        "{OBS_SOURCE}: #401 — the release-path erase no longer counts each discarded frame \
         (source->genlock_dropped_due++); silent drops are back. Every da_erase on the \
         ts-align release must increment genlock_dropped_due."
    );
    // Re-lock (stall catch-up) and late-hold (frame overdue but not arrived) events must
    // be counted too — the drop/hold classification the run-7020001 diagnosis lacked.
    for inc in ["source->genlock_relocks++", "source->genlock_late_holds++"] {
        assert!(
            src.contains(inc),
            "{OBS_SOURCE}: #401 — the cadence event counter `{inc}` is gone; the \
             relock/late-hold audit signal reverted. Re-apply."
        );
    }
}

#[test]
fn audit_line_exposes_cadence_counters() {
    // The 5s genlock-fifo audit line must surface the new counters (after backward_steps=,
    // existing fields untouched — scripts parse the line by field name) so a live loss is
    // visible in the OBS log alone (comprehensive-logging; the #401 loss was invisible).
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("backward_steps=%llu dropped_due=%llu relocks=%llu late_holds=%llu locked=%d"),
        "{OBS_SOURCE}: #401 — the genlock-fifo audit line no longer prints \
         dropped_due=/relocks=/late_holds=/locked= (after backward_steps=); release-path \
         frame loss is invisible in the log again. Re-apply."
    );
}

#[test]
fn backward_step_recovery_survives_the_cadence() {
    // #147/#269: the backward wall-clock step re-anchor (present OLDEST, preserve the
    // buffer, count once per event) must remain intact alongside the cadence — the
    // cadence must not regress it. (tests/genlock_preload.rs guards the full #147
    // contract probe-gated; this default-features guard pins the composition.)
    // #1009: the detection is the RE-QUALIFIED one (margin + sustain), and leaving a
    // regime must SELF-HEAL the configured hold (genlock_backward_regime_end zeroes the
    // locked boundary -> re-ACQUIRE) — this default-features guard is the ONLY local
    // (Tier-0-visible) anchor on the C tokens, so it pins the full #1009 set too.
    let src = squish(&vendor_file(OBS_SOURCE));
    for marker in [
        "max_ts > wall_now + backward_margin",
        "genlock_backward_step_margin_ns(",
        "#define GENLOCK_BACKWARD_STEP_MIN_MARGIN_NS 250000000ULL",
        "#define GENLOCK_BACKWARD_STEP_SUSTAIN_TICKS 3 ",
        "static void genlock_backward_regime_end(",
        "source->genlock_locked_next_boundary_ns = 0;",
        "genlock_backward_regime_end(source, reserve_ms);",
        "source->genlock_backward_regime_ticks++",
        "backward_regime_ticks=%llu",
        "source->genlock_backward_steps++",
        "source->genlock_in_backward_step = true",
    ] {
        assert!(
            src.contains(marker),
            "{OBS_SOURCE}: #401/#147/#1009 — the backward-step recovery marker `{marker}` is \
             gone; the cadence port regressed the #147/#269/#1009 re-anchor guard. Re-apply."
        );
    }
}

#[test]
fn steady_multi_consumes_at_an_integer_source_multiple_of_the_canvas() {
    // #726 — the live-event "like 15fps" judder. At a STRUCTURAL N>=2 source:canvas ratio (a
    // 60fps NDI camera into strih's 30fps canvas) the STEADY path presented the OLDEST matured
    // frame and re-anchored the boundary to it; one canvas interval (33_333_333 ns) lands a HAIR
    // under 2 source intervals (33_333_334 ns), so the boundary matured only ONE frame per tick
    // while N arrived — content crawled +1 source frame/tick, the queue grew, and the backlog
    // storm jumped +7 (~5x/s). The fix: for a structural N>=2 source (detected from the stamp
    // grid), mature every frame up to the boundary PLUS a half-interval slack, release the NEWEST
    // and retire the older matured one(s) into genlock_dropped_due — a uniform every-Nth-frame
    // cadence tracking real time, slew-immune (keys on the boundary, not the wall). N==1 keeps the
    // present-oldest lossless-drain path byte-identical. Behavioral RED->GREEN proof is
    // src/probe/genlock.rs `cadence_60_into_30_presents_uniform_every_second_frame`; this
    // default-features guard pins the C port so a subtree-pull or edit can't silently revert it.
    let src = squish(&vendor_file(OBS_SOURCE));
    // #726 STICKY-N (win5/win6 residual): the per-tick front-2 measurement is jitter-sensitive
    // (reads inconclusive on num<2 / a non-monotonic clock-step seam), so the detector is split
    // into a MEASURE (0 = inconclusive) and a STICKY effective helper that bridges an inconclusive
    // tick with the last confirmed multiple. Mirror: src/probe/genlock.rs
    // ReleaseCadence::measure_source_multiple / effective_source_multiple.
    assert!(
        src.contains("static inline uint32_t genlock_measure_source_multiple("),
        "{OBS_SOURCE}: #726 — the stamp-grid measurement (genlock_measure_source_multiple, \
         0 = inconclusive) is gone; the 60->30 STEADY cadence would crawl+jump again. Mirror: \
         src/probe/genlock.rs ReleaseCadence::measure_source_multiple."
    );
    assert!(
        src.contains("static inline uint32_t genlock_effective_source_multiple("),
        "{OBS_SOURCE}: #726 STICKY-N — the sticky effective-multiple helper \
         (genlock_effective_source_multiple, latch + bridge) is gone; an inconclusive front-2 tick \
         would fall back to the present-oldest CRAWL (win5/win6 residual). Mirror: \
         src/probe/genlock.rs ReleaseCadence::effective_source_multiple."
    );
    assert!(
        src.contains("source->genlock_last_known_n = fresh"),
        "{OBS_SOURCE}: #726 STICKY-N — a fresh measurement no longer LATCHES into \
         genlock_last_known_n; the sticky bridge can't remember the confirmed multiple. Re-apply."
    );
    assert!(
        src.contains("if (genlock_effective_source_multiple(source, interval) >= 2) {"),
        "{OBS_SOURCE}: #726 — the STEADY release no longer branches on the STICKY \
         genlock_effective_source_multiple(...) >= 2; the 60->30 multi-consume reverted to \
         present-oldest (the crawl) or to the jitter-sensitive per-tick check. Re-apply."
    );
    assert!(
        src.contains("mature_deadline")
            && src.contains("source->genlock_locked_next_boundary_ns + interval / 2"),
        "{OBS_SOURCE}: #726 — the STEADY N>=2 maturation slack (boundary + interval/2, so the \
         frame ~one canvas interval ahead matures despite canvas_interval being a hair under \
         N*src_interval) is gone; the multi-consume would mature only 1 frame and still crawl."
    );
    // The latch MUST be cleared on every GENUINE source-timeline discontinuity so a stale N cannot
    // outlive the rate it described. #741/#707 B2 changed the SET: the four sites are now
    // acquire / gap resync / backward clock-step / flush-inactive reset — the BACKLOG-STORM relock
    // is DELIBERATELY excluded (a queue-depth event is not a rate change; see
    // sticky_n_latch_lifecycle_and_robust_measure_741 below).
    let clears = src.matches("source->genlock_last_known_n = 0;").count();
    assert!(
        clears >= 4,
        "{OBS_SOURCE}: #726/#741 STICKY-N — the latch is cleared on only {clears} of the 4 required \
         source-timeline discontinuities (acquire / gap resync / backward clock-step / \
         flush-inactive reset); a stale N could outlive its rate. Re-apply the clears."
    );
}

#[test]
fn sticky_n_latch_lifecycle_and_robust_measure_741() {
    // #741 (#707 B2) — the CRAWL half of #707: a jittery 60-into-30 input crawled at +1 (window
    // uniform=0.481, histogram {1:295,2:407,3:102,7:39}) because the sticky-N detector kept
    // reading INCONCLUSIVE. Three code changes fix it; each is pinned here so a subtree-pull or
    // edit can't silently revert one. Behavioral RED→GREEN proof:
    // src/probe/genlock.rs `measure_scans_past_a_degenerate_front_pair_741` +
    // `backlog_relock_preserves_the_confirmed_multiple_741` (probe-gated); this default-features
    // guard pins the C port.
    let raw = vendor_file(OBS_SOURCE);

    // (a) The BACKLOG-STORM relock branch must NOT clear the latch — a queue-depth relock is NOT
    // evidence the source rate changed. Clearing it forced the next inconclusive tick to crawl
    // (N=1), re-growing the queue and re-triggering the relock: a self-sustaining crawl loop.
    let relock_pos = raw
        .find("source->genlock_relocks++;")
        .expect("#741: the backlog-storm relock branch (genlock_relocks++) must be present");
    // #940 piece 1: a fixed `relock_pos + 800` byte window is a PROXY for "the same relock
    // branch" and rots as the branch grows (the exact #859 lesson `ts_align_hold_counts_
    // as_hold_not_underrun` already documents above) — the #940 phase-evidence log line
    // pushed `release = due;` past the old 800-byte cap. Scope to the ENCLOSING FUNCTION
    // instead (up to the next top-level `static` definition), which cannot rot as the
    // branch grows.
    let window_end = raw[relock_pos..]
        .find("\nstatic ")
        .map(|rel| relock_pos + rel)
        .unwrap_or(raw.len());
    let after_relock = &raw[relock_pos..window_end];
    // #1003: the branch's terminating statement is now `release = sel_1003 + 1;` — the
    // phase-continuity selection replaced the newest-due one. The window this slices is
    // otherwise unchanged.
    let release_off = after_relock
        .find("release = sel_1003 + 1;")
        .expect("#741/#1003: the relock branch must end in the phase-continuity release (release = sel_1003 + 1;)");
    let relock_branch = &after_relock[..release_off];
    assert!(
        !relock_branch.contains("genlock_last_known_n = 0"),
        "{OBS_SOURCE}: #741/#707 B2 — the BACKLOG-STORM relock branch still clears \
         genlock_last_known_n; a queue-depth relock is NOT a rate change, and clearing there \
         re-crawls the next inconclusive tick and re-triggers the relock (self-sustaining crawl). \
         Mirror: src/probe/genlock.rs ReleaseCadence::tick backlog branch (no latch clear)."
    );

    // (b) genlock_measure_source_multiple must SCAN the first K queue entries for a
    // strictly-increasing pair (skipping a duplicate/degenerate front pair) — not read only
    // array[0..1]. The named scan-depth constant carries the rationale with the value.
    assert!(
        raw.contains("GENLOCK_MEASURE_SCAN_DEPTH"),
        "{OBS_SOURCE}: #741/#707 B2 — genlock_measure_source_multiple no longer scans the first K \
         queue entries (GENLOCK_MEASURE_SCAN_DEPTH) for a strictly-increasing pair; a duplicate \
         front stamp reads INCONCLUSIVE and the release crawls. Mirror: src/probe/genlock.rs \
         ReleaseCadence::measure_source_multiple."
    );

    // (c) The flush/inactive reset (frame==NULL: the source went inactive, delay line gone) must
    // CLEAR the latch — that is the genuinely-stale reset site. Pinned by co-location with the
    // sibling flush clears (genlock_filled / genlock_empty_run).
    let flush_pos = raw
        .find("source->async_active = false;")
        .expect("#741: the flush/inactive reset (async_active = false) must be present");
    let flush_tail = &raw[flush_pos..];
    let flush_end = flush_tail
        .find("free_async_cache(source);")
        .expect("#741: the flush/inactive block must end by freeing the async cache");
    let flush_block = &flush_tail[..flush_end];
    assert!(
        flush_block.contains("genlock_last_known_n = 0"),
        "{OBS_SOURCE}: #741/#707 B2 — the flush/inactive reset (frame==NULL) no longer clears \
         genlock_last_known_n; the source's whole delay line is gone there, so a resumed source \
         would trust a stale N. Add the clear next to genlock_filled / genlock_empty_run."
    );
}

#[test]
fn sticky_n_latch_field_declared_on_obs_source() {
    // #726 STICKY-N: the per-source confirmed-multiple latch must live on obs_source
    // (obs-internal.h), next to the other genlock cadence fields, so bzalloc zeroes it at create.
    let internal = squish(&vendor_file(OBS_INTERNAL));
    assert!(
        internal.contains("uint32_t genlock_last_known_n;"),
        "{OBS_INTERNAL}: #726 — the sticky-N latch field `uint32_t genlock_last_known_n;` is \
         missing from obs_source; the per-tick front-2 detector would crawl on inconclusive \
         ticks again (win5/win6 residual). Mirror: src/probe/genlock.rs ReleaseCadence::last_known_n."
    );
}

#[test]
fn slew_limited_settle_back_drain_present_and_wired_in_859() {
    // #859 follow-up: the latency-relative backlog threshold correctly stopped the
    // backlog-relock branch firing every tick in steady state, but that branch was ALSO the
    // FIFO's only mechanism for shedding excess queue depth after a genlock latency SETPOINT
    // INCREASE — with it gated off, the plain N==1 steady release (release=1/tick) held depth
    // CONSTANT forever (measured live: a +34 ms setpoint step produced +134 ms of actual delay,
    // stable across 6 samples). This bounded, additional drain is what converges it back down.
    // A `git subtree pull` or edit reverting any of these pieces would silently reintroduce the
    // parked-overshoot regression, so pin the whole three-file mirror here.
    let internal = squish(&vendor_file(OBS_INTERNAL));
    assert!(
        internal.contains("uint64_t genlock_ticks_since_drain;"),
        "{OBS_INTERNAL}: #859 follow-up — the drain rate-limit counter \
         `uint64_t genlock_ticks_since_drain;` is missing from obs_source; the slew-limited \
         drain has no way to bound its own rate. Mirror: src/probe/genlock.rs \
         ReleaseCadence::ticks_since_last_drain."
    );

    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("#define GENLOCK_DRAIN_HYSTERESIS_FRAMES 2"),
        "{OBS_SOURCE}: #859 follow-up — the drain hysteresis constant \
         (GENLOCK_DRAIN_HYSTERESIS_FRAMES, must stay 2) is missing or changed; without it \
         ordinary arrival jitter around the target would trigger spurious drains. Mirror: \
         src/genlock_backlog.rs DRAIN_HYSTERESIS_FRAMES (Tier-0 unit-tested)."
    );
    assert!(
        src.contains("#define GENLOCK_DRAIN_MIN_TICK_INTERVAL 30"),
        "{OBS_SOURCE}: #859 follow-up — the drain rate-limit constant \
         (GENLOCK_DRAIN_MIN_TICK_INTERVAL, must stay 30) is missing or changed; without it the \
         drain could reproduce the every-tick backlog-relock burst it exists to avoid. Mirror: \
         src/genlock_backlog.rs DRAIN_MIN_TICK_INTERVAL."
    );
    assert!(
        src.contains("static bool genlock_should_drain_one("),
        "{OBS_SOURCE}: #859 follow-up — the slew-limited drain decision helper \
         (genlock_should_drain_one) is gone; the FIFO would park at a setpoint-change overshoot \
         indefinitely again. Mirror: src/genlock_backlog.rs should_drain_one (Tier-0 tested) / \
         src/probe/genlock.rs ReleaseCadence::should_drain_one."
    );
    // Wired IN, not merely defined — the same "assert the call site, not just the helper"
    // discipline the #859 backlog-threshold test above already applies.
    assert!(
        src.contains("genlock_should_drain_one(source, reserve_ms, interval)"),
        "{OBS_SOURCE}: #859 follow-up — genlock_should_drain_one is defined but no longer \
         CALLED from the STEADY N==1 release path; the drain would be dead code and the queue \
         would park at a setpoint-change overshoot indefinitely again."
    );
    // The drain must drop the CURRENT oldest (array[0]) and let the array shift — NOT drop
    // array[1] while leaving array[0] untouched. The latter desyncs the re-anchored boundary
    // from the real evenly-spaced frame timeline (confirmed by simulation: the very next tick
    // reads as a HOLD and a GAP RESYNC regains exactly what the drain shed — a self-cancelling
    // no-op). A subtree-pull or a "simplify this" edit reverting to array[1] would silently
    // regress the fix back to a no-op while still LOOKING like it works (still compiles, still
    // counts genlock_dropped_due). Checked on the RAW (non-squished) source — squish() would
    // erase the tab/newline structure this needs.
    assert!(
        raw_drain_erases_index_zero(&vendor_file(OBS_SOURCE)),
        "{OBS_SOURCE}: #859 follow-up — the drain no longer erases array[0] (the current \
         oldest) before re-reading next_frame = array[0]; dropping array[1] instead is a \
         self-cancelling no-op (see the comment above genlock_should_drain_one's call site)."
    );
}

/// Structural check (not a literal-whitespace match) for the previous assertion: within the
/// drain's own `if` block, the FIRST `da_erase(source->async_frames, N)` call must erase index
/// `0`, not `1` — confirms the drop-older/present-newest fix regardless of exact formatting.
fn raw_drain_erases_index_zero(raw: &str) -> bool {
    let Some(drain_pos) = raw.find("genlock_should_drain_one(source, reserve_ms, interval) &&")
    else {
        return false;
    };
    let window_end = (drain_pos + 600).min(raw.len());
    let window = &raw[drain_pos..window_end];
    window.contains("da_erase(source->async_frames, 0);") && !window.contains("array[1]")
}

/// #1049 — the bounded PHASE CONVERGENCE on the steady conveyor must be present and WIRED. The
/// N>=2 conveyor has no depth drain, so without this a per-camera acquire-phase error persisted
/// forever (the strih 60-into-30 A/V-offset ladder). Mirror of src/genlock_backlog.rs
/// should_converge_phase (Tier-0 tested) + the C-vs-Rust parity gate
/// tests/genlock_relock_selection_parity.rs.
#[test]
fn phase_convergence_present_and_wired_in_1049() {
    let src = squish(&vendor_file(OBS_SOURCE));

    // The PURE liftable decision (the parity gate compiles this standalone) and its source wrapper.
    assert!(
        src.contains("static inline bool genlock_phase_converge_due("),
        "{OBS_SOURCE}: #1049 — the pure phase-converge decision genlock_phase_converge_due is \
         gone; the per-camera acquire-phase would never converge again. Mirror: \
         src/genlock_backlog.rs should_converge_phase."
    );
    assert!(
        src.contains("static bool genlock_should_converge_phase("),
        "{OBS_SOURCE}: #1049 — the source wrapper genlock_should_converge_phase is gone."
    );
    // The quantum is the SOURCE interval (interval/n), not a bare canvas interval — the tightest
    // threshold that still clears the natural steady phase (the #998 lesson applied to phase).
    assert!(
        src.contains("const uint64_t quantum = interval_ns / nn;"),
        "{OBS_SOURCE}: #1049 — the shed quantum must be the SOURCE interval (interval_ns / n); a \
         bare canvas interval would leave the n=2 threshold a full frame too high."
    );
    // The target is max(reserve, FLOOR) — the floor being the freshest queued frame's age. A
    // reserve-only target ignores the transport skew and limit-cycles at the rig's ~20 ms skew on
    // shallow reserves (the #998 class, review finding).
    assert!(
        src.contains("const uint64_t floor_ns = wall_now_ns > newest_stamp_ns ? wall_now_ns - newest_stamp_ns : 0;"),
        "{OBS_SOURCE}: #1049 — the achievable floor (wall - newest_stamp) is gone; a reserve-only \
         threshold re-creates the #998 drop/regain limit cycle at the rig's transport skew."
    );
    assert!(
        src.contains("const uint64_t target = reserve_ns > floor_ns ? reserve_ns : floor_ns;"),
        "{OBS_SOURCE}: #1049 — the converge target must be max(reserve, floor), never reserve alone."
    );
    // N>=2-ONLY gate (coordinator's live finding): an N==1 phase shed does not stick and
    // limit-cycles on a deep source; convergence must early-return for n<2.
    assert!(
        src.contains("if (n < 2) return false;"),
        "{OBS_SOURCE}: #1049 — the N>=2-only gate (if (n < 2) return false;) is gone; convergence \
         would limit-cycle on a deep N==1 source (the stream NDI 2ME PGM oscillation)."
    );
    // The shed is CALLED from the release tail, gated on converge_eligible.
    assert!(
        src.contains("bool converge_eligible = false;"),
        "{OBS_SOURCE}: #1049 — the converge_eligible gate is gone from genlock_release_tick."
    );
    assert!(
        src.contains("genlock_should_converge_phase(source, reserve_ms, interval, wall_now)"),
        "{OBS_SOURCE}: #1049 — genlock_should_converge_phase is defined but no longer CALLED from \
         the release tail; the shed would be dead code and the phase would never converge."
    );
    // Both STEADY presents (N==1 and N>=2) mark themselves converge_eligible — the N>=2 path is
    // the one with no depth drain at all.
    assert_eq!(
        src.matches("converge_eligible = true;").count(),
        2,
        "{OBS_SOURCE}: #1049 — both STEADY branches (N==1 and N>=2) must set converge_eligible; \
         the N>=2 conveyor has no other restoring force toward configured."
    );
    // The shed drops the CURRENT would-be-presented frame (index 0) and presents the next — the
    // same drop-older/present-fresher idiom the #859 drain uses (never a snap-back no-op).
    assert!(
        raw_converge_erases_index_zero(&vendor_file(OBS_SOURCE)),
        "{OBS_SOURCE}: #1049 — the converge shed must erase array[0] (the would-be-presented \
         frame) and present the next; dropping the one BEHIND the presented frame is the \
         self-cancelling no-op the drain call-site comment documents."
    );
    // On the N>=2 path the drain block did NOT run, so the converge block MUST maintain the shared
    // throttle counter itself — without this increment the feature is silently DEAD on its primary
    // target (the 60-into-30 ingests): the counter never reaches the interval and the shed never
    // fires (review finding 🟡3).
    assert!(
        src.contains("} else if (!drain_eligible) { source->genlock_ticks_since_drain++;"),
        "{OBS_SOURCE}: #1049 — the N>=2 converge branch no longer maintains the shared throttle \
         counter (else if (!drain_eligible) ...++); the shed would never fire on a 60-into-30 \
         source — the feature dead in exactly its primary target."
    );
    // A converge shed increments a DISTINCT counter for the audit line — a converge shed is
    // otherwise indistinguishable from any other drop (the genlock-hold-collapse playbook lesson).
    assert!(
        src.contains("source->genlock_converge_sheds++;"),
        "{OBS_SOURCE}: #1049 — the distinct converge-shed observability counter is gone."
    );
    assert!(
        src.contains("converge_sheds=%u"),
        "{OBS_SOURCE}: #1049 — the audit line no longer reports converge_sheds= (post-deploy \
         verification of this ticket reads it: the shed must fire, then go quiet)."
    );
}

/// Structural check: within the #1049 converge block, the FIRST `da_erase` erases index 0.
fn raw_converge_erases_index_zero(raw: &str) -> bool {
    let Some(pos) =
        raw.find("genlock_should_converge_phase(source, reserve_ms, interval, wall_now) &&")
    else {
        return false;
    };
    let window_end = (pos + 600).min(raw.len());
    let window = &raw[pos..window_end];
    window.contains("da_erase(source->async_frames, 0);") && !window.contains("array[1]")
}

#[test]
fn relock_events_log_phase_evidence_940() {
    // #940 piece 1 — INSTRUMENT each backlog-relock event with the phase evidence needed
    // to attribute a future ±1–2-frame A/V-offset step to (or rule it out from) a
    // specific relock: current depth, steady_depth_frames, due count, erased count, head
    // skew, and the deadline's own remainder mod the frame interval (the "wall-grid
    // phase" — piece 3 drives this to a fixed value; today it wanders, which IS the
    // hidden phase state #940 is chasing). Logged ONCE PER EVENT inside the
    // BACKLOG-STORM relock branch (genlock_relocks++), not folded into the periodic 5s
    // audit line (which samples a snapshot, not a per-event trace). A subtree pull or
    // edit dropping this silently removes the only per-event evidence #940's analysis
    // depends on.
    let raw = vendor_file(OBS_SOURCE);
    let relock_pos = raw
        .find("source->genlock_relocks++;")
        .expect("#940: the backlog-storm relock branch (genlock_relocks++) must be present");
    let release_off = raw[relock_pos..]
        .find("release = sel_1003 + 1;")
        .expect("#940/#1003: the relock branch must end in the phase-continuity release (release = sel_1003 + 1;)");
    let relock_branch = &raw[relock_pos..relock_pos + release_off];
    assert!(
        relock_branch.contains("genlock-relock"),
        "{OBS_SOURCE}: #940 piece 1 — the per-event relock log line (\"genlock-relock\") \
         is gone from the backlog-storm branch; re-apply the phase-evidence instrumentation."
    );
    let squished = squish(relock_branch);
    for field in [
        "depth=%zu",
        "steady_depth_frames=%zu",
        "due=%zu",
        "erased=%zu",
        "head_skew_ms=%lld",
        // #1003: wall_grid_phase_ns was structurally BLIND — it logged `present_ts %%
        // interval` while #940 piece 3 floors present_ts to that very interval, so a
        // floored value mod its own divisor is IDENTICALLY 0 on every run at every
        // latency. These three replace it and are the live post-deploy evidence fields.
        "tick_phase_ns=%llu",
        "anchor_ns=%llu",
        "sel_vs_newest_due=%lld",
    ] {
        assert!(
            squished.contains(field),
            "{OBS_SOURCE}: #940 piece 1 — the relock phase-evidence log line is missing \
             field `{field}`; re-apply the full instrumentation (depth / \
             steady_depth_frames / due / erased / head_skew_ms / tick_phase_ns / \
             anchor_ns / sel_vs_newest_due)."
        );
    }
    // #940 piece 1 correctness fix: the logged steady_depth_frames must subtract the FULL
    // scaled margin (piece 2: GENLOCK_QDEPTH_RELOCK_MARGIN * n) that
    // genlock_backlog_relock_qdepth() now returns, not the bare margin — else a 60-into-30
    // source (n>=2) logs an inflated steady_depth_frames by MARGIN*(n-1). Anchored on the
    // exact scaled-subtraction expression so a future edit that reverts to a bare-margin
    // subtraction (correct pre-piece-2, wrong once the margin is scaled) is caught.
    assert!(
        squished.contains("(size_t)GENLOCK_QDEPTH_RELOCK_MARGIN * (size_t)n_for_log"),
        "{OBS_SOURCE}: #940 piece 1 — the relock log's steady_depth_frames computation no \
         longer subtracts the source-multiple-scaled margin (GENLOCK_QDEPTH_RELOCK_MARGIN * \
         n_for_log); it would log an inflated value on every 60-into-30 source once piece 2's \
         scaled margin is in effect."
    );
}

#[test]
fn ts_align_deadline_is_phase_pinned_to_the_wall_grid_940() {
    // #940 piece 3 — the structural fix. The pre-#940 ts-align reserve deadline was a raw
    // continuous quantity (wall_now - reserve), making "which frame releases now" a
    // function of the EXACT sub-ms instant a lock/relock happens to fire -- a hidden
    // per-lock-episode phase, re-sampled on every ACQUIRE/RELOCK, observed live as a
    // ±1-2-frame A/V-offset step between lock episodes at deep latency. Quantizing the
    // deadline to the canvas frame GRID (floor(deadline/interval)*interval) makes it a
    // pure function of wall time instead. The pure floor/hysteresis math is Tier-0
    // unit-tested in src/genlock_backlog.rs; this guards the C wiring.
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains("static inline uint64_t genlock_phase_pin_deadline("),
        "{OBS_SOURCE}: #940 piece 3 — the grid-quantization helper \
         (genlock_phase_pin_deadline) is gone; the deadline reverted to the raw continuous \
         quantity that re-samples a different phase every lock episode. Mirror: \
         src/genlock_backlog.rs phase_pinned_deadline (Tier-0 unit-tested)."
    );
    // The floor-to-grid ARITHMETIC itself (not just the helper's existence) — a subtree
    // pull or a "simplify this" edit could keep the helper but neuter its body.
    assert!(
        src.contains("(deadline_ns / interval_ns) * interval_ns"),
        "{OBS_SOURCE}: #940 piece 3 — genlock_phase_pin_deadline no longer computes the \
         floor-to-grid quotient (deadline_ns / interval_ns) * interval_ns; re-apply."
    );
    // Wired IN at the ts-align call site for the reserve_ms>0 (deep-latency, ms-granular)
    // path — only defining the helper without calling it is dead code (same "assert the
    // call site, not just the helper" discipline #859 already applies).
    assert!(
        src.contains("present_ts = genlock_phase_pin_deadline(present_ts, interval);"),
        "{OBS_SOURCE}: #940 piece 3 — genlock_phase_pin_deadline is defined but not CALLED \
         from the ts-align reserve-ms release path; the deadline would stay un-quantized \
         and the deep-latency phase step would persist."
    );
    // The hysteresis slack on the grid comparison — without it, a frame captured
    // essentially exactly on a grid line flips due/not-due from ordinary sub-ms
    // render-tick jitter on the floor division (the design's own documented risk).
    assert!(
        src.contains("#define GENLOCK_PHASE_PIN_HYSTERESIS_NS 5000000ULL"),
        "{OBS_SOURCE}: #940 piece 3 — the grid-comparison hysteresis constant \
         (GENLOCK_PHASE_PIN_HYSTERESIS_NS, must stay 5ms) is missing or changed; without \
         it a frame landing exactly on a grid line could flap due/not-due. Mirror: \
         src/genlock_backlog.rs PHASE_PIN_HYSTERESIS_NS."
    );
    assert!(
        src.contains("array[due]->timestamp <= present_ts + due_hysteresis_ns"),
        "{OBS_SOURCE}: #940 piece 3 — the due-computation loop no longer applies the \
         grid-comparison hysteresis (due_hysteresis_ns); re-apply."
    );
    // Gated to the reserve_ms>0 path ONLY — the (effectively unused on this build's
    // #257-floored latency) frame-count preload path must stay byte-identical.
    assert!(
        src.contains("reserve_ms > 0 ? GENLOCK_PHASE_PIN_HYSTERESIS_NS : 0"),
        "{OBS_SOURCE}: #940 piece 3 — the grid hysteresis is no longer gated to the \
         reserve_ms>0 (ms-granular deep-latency) path; the frame-count preload path must \
         stay byte-identical to its pre-#940 behaviour."
    );
}

#[test]
fn relock_selection_is_phase_anchored_not_newest_due_1003() {
    // #1003 — the structural fix #940 piece 3 could not deliver on its own. The grid pin
    // removed the deadline's dependence on the exact instant a relock fired, but the
    // release PHASE is minted by the SELECTION, and that stayed an instant-sampled,
    // STATELESS newest-due comparison carrying two independently flippable edges: the
    // deadline floors to the RECEIVER grid (±2 ms of render-tick slew near the floor's step
    // point moves the whole pinned cell), while the stamps compared against it sit on the
    // SENDER's 33,333,300 ns grid (a 33 ns/frame beat, so the fixed 5 ms hysteresis is a
    // FIXED edge inside a DRIFTING relative phase). Two edges = up to four outcomes
    // spanning two frames = the measured −64.5 / +56..63 ms per-episode steps.
    //
    // The selection arithmetic itself is Tier-0 unit-tested in src/genlock_backlog.rs
    // (relock_select_nearest / relock_anchor_age_ns / phase_anchor_from_present, incl. the
    // whole-frame-step defect lock); this guards the C WIRING, which compiles on CI only.
    let src = squish(&vendor_file(OBS_SOURCE));
    let internal = squish(&vendor_file(OBS_INTERNAL));

    // (a) the per-source anchor state exists.
    assert!(
        internal.contains("uint64_t genlock_phase_anchor_ns;"),
        "{OBS_INTERNAL}: #1003 — the per-source phase anchor (genlock_phase_anchor_ns) is \
         gone; without remembered state the relock has nothing to inherit a phase FROM and \
         necessarily re-samples one. Mirror: src/genlock_backlog.rs relock_anchor_age_ns."
    );

    // (b) the selection helper exists AND its nearest-neighbour body is intact — a
    // "simplify this" edit could keep the helper and neuter it back into an edge.
    assert!(
        src.contains("static inline size_t genlock_relock_select_nearest("),
        "{OBS_SOURCE}: #1003 — the phase-continuity selection helper \
         (genlock_relock_select_nearest) is gone; the relock reverted to instant-sampled \
         selection. Mirror: src/genlock_backlog.rs relock_select_nearest."
    );
    assert!(
        // SHORT + wrap-independent on purpose: a clang-format bump or a loop-variable
        // rename must not fail this gate with a misleading message (the anchor-fragility
        // lesson #940 records for byte windows applies to long literals too).
        src.contains("best_d = genlock_abs_diff_ns("),
        "{OBS_SOURCE}: #1003 — genlock_relock_select_nearest no longer measures each queued \
         stamp's DISTANCE from the anchor target; a selection that is not \
         nearest-neighbour is not continuous, and a non-continuous rule has an edge for \
         slew or the sender-grid beat to flip."
    );
    assert!(
        src.contains("if (d < best_d)"),
        "{OBS_SOURCE}: #1003 — the nearest scan's STRICT `<` is gone. A non-strict compare \
         resolves ties toward the NEWER frame, which lets an exactly-equidistant target \
         oscillate between neighbours on successive episodes — the very failure this \
         function exists to remove. Mirror: src/genlock_backlog.rs relock_select_nearest."
    );
    assert!(
        src.contains("return anchor > configured ? anchor : configured;"),
        "{OBS_SOURCE}: #1003 — genlock_relock_target_age_ns no longer returns the tracked \
         anchor FLOORED at the configured latency. Without the anchor it targets a fixed \
         latency instead of the conveyor's real on-air age; without the floor a degenerate \
         or stale anchor below the hold targets the live edge and one relock erases the \
         entire delay line. Mirror: src/genlock_backlog.rs relock_anchor_age_ns."
    );

    // (c) BOTH relock branches are wired in — ACQUIRE and BACKLOG STORM. Defining the
    // helper without calling it from both is dead code (the same "assert the call site,
    // not just the helper" discipline #859/#940 already apply here).
    assert!(
        src.contains("release = genlock_relock_select_nearest(source, wall_now, reserve_ms) + 1;"),
        "{OBS_SOURCE}: #1003 — the ACQUIRE branch no longer selects by phase continuity; a \
         cold/self-heal re-acquire would re-mint an edge-ridden phase."
    );
    assert!(
        src.contains("release = sel_1003 + 1;"),
        "{OBS_SOURCE}: #1003 — the BACKLOG-STORM branch no longer selects by phase \
         continuity. This is the branch the live evidence traced the episode steps to \
         (relocks=13 in ~3.8 h on the deployed build)."
    );
    assert!(
        !src.contains("release = due;"),
        "{OBS_SOURCE}: #1003 — a relock branch still uses the newest-due selection \
         (`release = due;`). That rule is instant-sampled and stateless: it re-mints the \
         release phase on every lock episode, which is the whole defect. The Tier-0 lock \
         `instant_sampled_selection_steps_a_whole_frame_at_the_grid_edges_1003` shows ONE \
         NANOSECOND of render-tick slew moving it a whole frame."
    );

    // (d) the `release - 1` erase idiom is untouched, so a relock still sheds DEPTH.
    assert!(
        src.contains("size_t to_drop = release - 1;"),
        "{OBS_SOURCE}: #1003 — the erase-into-dropped_due idiom (to_drop = release - 1) is \
         gone; the relock would stop shedding the queue depth a stall's burst built up, \
         leaving the FIFO parked overshot (the issue-859 branch would become decorative)."
    );

    // (e) the anchor is updated on the conveyor's presents ONLY. A relock that WRITES the
    // anchor re-mints a phase from whatever frame it happened to select — the defect,
    // reintroduced through the back door.
    assert!(
        src.contains("if (anchor_update) source->genlock_phase_anchor_ns ="),
        "{OBS_SOURCE}: #1003 — the phase anchor is no longer updated (gated by \
         anchor_update) on the shared present tail. Ungated, the ACQUIRE/BACKLOG relock \
         presents would redefine the anchor they are supposed to INHERIT."
    );
    assert!(
        src.contains("genlock_phase_anchor_from_present("),
        "{OBS_SOURCE}: #1003 — the anchor is no longer derived via \
         genlock_phase_anchor_from_present (the saturating wall_now - presented_ts)."
    );

    // (f) the seams. A stepped wall clock invalidates every sampled age by exactly the
    // step, and a flush destroys the delay line the age described.
    let regime_end_all = src
        .split("static void genlock_backward_regime_end(")
        .nth(1)
        .expect("#1003: genlock_backward_regime_end must exist (issue 1009 self-heal)");
    // Scope to the ENCLOSING FUNCTION (up to the next top-level definition), never a fixed
    // byte window — the exact lesson #940 piece 1 already records above: a byte cap is a
    // PROXY for "the same function" and rots the moment the body grows.
    let regime_end = regime_end_all
        .find("static ")
        .map_or(regime_end_all, |i| &regime_end_all[..i]);
    assert!(
        regime_end.contains("source->genlock_phase_anchor_ns = 0;"),
        "{OBS_SOURCE}: #1003 — a backward-step regime end no longer CLEARS the phase \
         anchor. The receiver wall clock moved by the step, so re-acquiring against a \
         pre-step age re-establishes the hold at a phase off by the whole clock step — \
         while that function's own contract is to re-acquire the CONFIGURED hold."
    );
    // EVERY site that destroys the delay line must clear the anchor — the explicit flush,
    // the overrun force-drain, AND the async_texture_changed re-alloc. Counting them
    // against the call sites is what stops the seam list silently drifting when a future
    // edit adds a fourth (only the flush was covered on the first cut of #1003).
    let seams = src
        .matches("source->genlock_phase_anchor_ns = 0; free_async_cache(source);")
        .count();
    let frees = src.matches("free_async_cache(source);").count();
    assert_eq!(
        seams, frees,
        "{OBS_SOURCE}: #1003 — {frees} site(s) call free_async_cache() but only {seams} \
         clear the phase anchor first. Each one destroys the whole delay line, so a relock \
         firing before the next STEADY/GAP present would target an age describing a delay \
         line that no longer exists."
    );

    // A latency SETPOINT change also invalidates the remembered age. Without this the
    // relock sheds NOTHING after a decrease while the lowered threshold qualifies the
    // backlog branch every tick — a permanent relock storm that ALSO starves the
    // settle-back drain, which only runs on the STEADY path that branch pre-empts.
    // Scoped to the setter function, never a byte window.
    let setter_all = src
        .split("void obs_source_set_genlock_latency_ms(")
        .nth(1)
        .expect("#1003: obs_source_set_genlock_latency_ms must exist");
    let setter = setter_all
        .find("uint32_t obs_source_get_genlock_latency_ms(")
        .map_or(setter_all, |i| &setter_all[..i]);
    assert!(
        setter.contains("source->genlock_phase_anchor_ns = 0;"),
        "{OBS_SOURCE}: #1003 — a latency setpoint change no longer clears the phase anchor. \
         The remembered age describes a hold that no longer exists; on a DECREASE the relock \
         then sheds nothing forever while the backlog branch fires every tick. Tier-0 lock: \
         latency_setpoint_decrease_converges_without_a_relock_storm_1003."
    );

    // The stale-anchor self-heal: a BACKLOG relock that would shed nothing is proof the
    // anchor cannot describe a queue this deep.
    assert!(
        src.contains("if (sel_1003 == 0 && source->genlock_phase_anchor_ns != 0)"),
        "{OBS_SOURCE}: #1003 — the BACKLOG branch's stale-anchor self-heal is gone. Without \
         it a relock that sheds zero frames re-fires every tick (the branch pre-empts \
         STEADY, so the settle-back drain never runs either) — the useless-`relocks`-counter \
         state issue 859 removed, reintroduced."
    );

    // (g) the `due` scan and its hysteresis stay BYTE-IDENTICAL — they still QUALIFY
    // due-ness (and still gate the backlog branch + the audit), they simply no longer
    // SELECT. #1003 deliberately changes selection only.
    assert!(
        src.contains("array[due]->timestamp <= present_ts + due_hysteresis_ns"),
        "{OBS_SOURCE}: #1003 — the due prefix scan changed. #1003 must leave due-ness \
         qualification (and therefore the backlog trigger and the audit sample) exactly as \
         #940 piece 3 left it; only the SELECTION moves."
    );
}

#[test]
fn backlog_relock_margin_scales_with_the_source_multiple_940() {
    // #940 piece 2 — arrival-surplus-aware relock threshold. genlock_backlog_relock_qdepth()
    // already MEASURES the source's own rate multiple `n` (for the steady-depth SOURCE-rate
    // scaling); piece 2 reuses that SAME `n` to scale the MARGIN too, so a 60-into-30 camera
    // ingest (n=2) stops relocking on routine arrival surplus at its shallow per-source
    // latency, while a 30-into-30 source (n=1) stays byte-identical.
    let src = squish(&vendor_file(OBS_SOURCE));
    assert!(
        src.contains(
            "return (size_t)(depth + (uint64_t)GENLOCK_QDEPTH_RELOCK_MARGIN * (uint64_t)n);"
        ),
        "{OBS_SOURCE}: #940 piece 2 — genlock_backlog_relock_qdepth no longer scales the \
         margin by the measured source multiple n; re-apply. Mirror: \
         src/genlock_backlog.rs backlog_relock_threshold (Tier-0 unit-tested)."
    );
    // The OLD flat-add form (margin never scaled) must be GONE — a subtree pull or a
    // "simplify this" edit reverting to it would silently reintroduce the #940 churn on
    // every 60-into-30 camera ingest.
    assert!(
        !src.contains("return (size_t)(depth + GENLOCK_QDEPTH_RELOCK_MARGIN);"),
        "{OBS_SOURCE}: #940 piece 2 — the OLD unscaled-margin return is BACK; the arrival-\
         -surplus-aware scaling reverted."
    );
    // The margin CONSTANT itself must stay the original 6 — piece 2 changed what the margin
    // is MULTIPLIED BY, not the margin's own value (same discipline #859's own test already
    // applies to this constant).
    assert!(
        src.contains("#define GENLOCK_QDEPTH_RELOCK_MARGIN 6"),
        "{OBS_SOURCE}: #940 piece 2 — the backlog MARGIN constant is no longer the original \
         6; #940 scales the margin BY the source multiple, it must never also widen the \
         constant itself."
    );
}

#[test]
fn release_cadence_extracted_into_genlock_release_tick_1038() {
    // #1038: ready_async_frame() had grown to ~832 lines with the whole #401 phase-locked
    // release cadence inlined. The cadence (ACQUIRE / BACKLOG-STORM / STEADY / GAP-RESYNC /
    // HOLD chain, the to_drop erase loop, the #859 settle-back drain, the present tail) was
    // extracted VERBATIM into its own `static bool genlock_release_tick(...)` so the next
    // cadence ticket does not add to an already-oversized function again. Pure structural
    // move, zero behaviour change. This gate pins the boundary so a subtree pull or a
    // "re-inline it" edit can't silently undo the extraction (and so every OTHER anchor in
    // this file that slices on the enclosing function keeps resolving inside the new one).
    let raw = vendor_file(OBS_SOURCE);

    // (a) The extracted function must EXIST.
    assert!(
        raw.contains("static bool genlock_release_tick("),
        "{OBS_SOURCE}: #1038 — the extracted `static bool genlock_release_tick(` is gone; \
         the #401 cadence was re-inlined into ready_async_frame(). Re-extract it."
    );

    // (b) ready_async_frame must CALL it (a tail return), not carry the cadence inline. The
    //     call passes the six cadence inputs + now_ns; anchor on the stable head of the call.
    assert!(
        squish(&raw).contains("return genlock_release_tick(source, wall_now, present_ts, due,"),
        "{OBS_SOURCE}: #1038 — ready_async_frame no longer tail-calls genlock_release_tick \
         with the cadence inputs; the extraction was reverted or the call site changed shape."
    );

    // (c) genlock_release_tick must be defined BEFORE ready_async_frame — both so C sees it
    //     without a forward declaration AND so the enclosing-function slice anchors in this
    //     file (e.g. the #741 `genlock_relocks++` → next `\nstatic ` window) resolve INSIDE
    //     genlock_release_tick rather than spilling past ready_async_frame.
    let tick_pos = raw
        .find("static bool genlock_release_tick(")
        .expect("#1038: genlock_release_tick must be present");
    let ready_pos = raw
        .find("static bool ready_async_frame(obs_source_t *source, uint64_t sys_time)\n{")
        .expect("#1038: ready_async_frame definition must be present");
    assert!(
        tick_pos < ready_pos,
        "{OBS_SOURCE}: #1038 — genlock_release_tick must be DEFINED before ready_async_frame \
         (it is only called from there and several static-boundary anchors depend on its \
         relock branch preceding ready_async_frame's own `static ` line)."
    );

    // (d) The relock branch's terminal statement (`release = sel_1003 + 1;`) must now live
    //     inside genlock_release_tick — i.e. between its definition and ready_async_frame's.
    //     This is the exact window the #741 anchor slices; pin it here too so the invariant
    //     is explicit at the extraction boundary.
    let between = &raw[tick_pos..ready_pos];
    assert!(
        between.contains("release = sel_1003 + 1;")
            && between.contains("source->genlock_relocks++;"),
        "{OBS_SOURCE}: #1038 — the BACKLOG-STORM relock branch (genlock_relocks++ … \
         release = sel_1003 + 1;) is no longer contained within genlock_release_tick; the \
         #741/#940 enclosing-function slice anchors would resolve past ready_async_frame."
    );
}

#[test]
fn acquire_bracketing_gate_1161() {
    // #1161 — the Stage-2 ACQUIRE bracketing gate + the pin-rise re-acquire that triggers it.
    // (a) the setter forces a re-acquire on a pin RISE by zeroing the conveyor boundary + the
    // bracket counter; (b) the ACQUIRE branch (N>=2) holds via genlock_relock_acquire_should_hold
    // until the queue deepens to the raised reserve; (c) the pure helper + its fail-open #define
    // exist and are consumed. The behavioral RED->GREEN + the executable C-vs-Rust parity live in
    // src/genlock_backlog.rs (Tier-0 unit-tested) + tests/genlock_relock_selection_parity.rs; this
    // default-features guard pins the C PORT so a subtree-pull or edit can't silently revert it
    // (the vendored C compiles only on CI). Mirror: src/genlock_backlog.rs relock_acquire_should_hold.
    let src = squish(&vendor_file(OBS_SOURCE));
    let internal = squish(&vendor_file(OBS_INTERNAL));

    // (a) setter re-acquire on a pin RISE — the frame-mover's trigger (issue 1161 root cause).
    assert!(
        src.contains(
            "if (clamped > prev) { source->genlock_locked_next_boundary_ns = 0; \
             source->genlock_acquire_bracket_ticks = 0; }"
        ),
        "{OBS_SOURCE}: #1161 — obs_source_set_genlock_latency_ms no longer zeroes the conveyor \
         boundary on a pin RISE; a raised per-source pin can never re-acquire, so the presented \
         frame never moves deeper (the #1161 residual). Re-apply."
    );

    // (b) the pure decision + its fail-open margin, mirrored from src/genlock_backlog.rs.
    assert!(
        src.contains("#define GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS 3ULL"),
        "{OBS_SOURCE}: #1161 — the fail-open margin GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS is gone."
    );
    assert!(
        src.contains("static inline bool genlock_relock_acquire_should_hold("),
        "{OBS_SOURCE}: #1161 — the pure ACQUIRE bracketing gate helper \
         genlock_relock_acquire_should_hold is gone; the frame-mover reverted. Mirror: \
         src/genlock_backlog.rs relock_acquire_should_hold."
    );
    assert!(
        src.contains(
            "const uint64_t cap = (reserve_ns + interval_ns - 1) / interval_ns + \
             GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS;"
        ),
        "{OBS_SOURCE}: #1161 — the fail-open cap ceil(reserve/interval)+margin is gone from the \
         gate; a queue that never deepens could hold forever (a new hold-collapse mode). Re-apply."
    );

    // (c) the gate is WIRED into the ACQUIRE branch, N>=2 only, feeding + incrementing the counter.
    assert!(
        src.contains("genlock_relock_acquire_should_hold(oldest_age,")
            && src.contains("source->genlock_acquire_bracket_ticks++;"),
        "{OBS_SOURCE}: #1161 — the ACQUIRE branch no longer calls \
         genlock_relock_acquire_should_hold (with oldest_age) / increments \
         genlock_acquire_bracket_ticks; the bracketing hold is gone and a forced re-acquire would \
         land one canvas frame below the raised target. Re-apply."
    );

    // The new remembered-state field must live on obs_source (bzalloc-zeroed with the counters).
    assert!(
        internal.contains("uint32_t genlock_acquire_bracket_ticks;"),
        "{OBS_INTERNAL}: #1161 — the ACQUIRE bracket-tick counter field \
         genlock_acquire_bracket_ticks is missing from obs_source; the fail-open cap has nowhere \
         to count. Re-apply."
    );

    // (d) REMEMBERED-STATE SEAM completeness (vendored-libobs-change-safety.md "Adding REMEMBERED
    // STATE"): genlock_acquire_bracket_ticks is a per-source field that survives across ACQUIRE
    // ticks, so it MUST be zeroed at every boundary-invalidation seam that begins a fresh acquire
    // episode — else a stale count undercuts the next re-acquire's fail-open cap (a shallow lock).
    // The seams are the same ones that clear genlock_phase_anchor_ns (the #1003 seam guard above).
    // (d.1) EVERY free_async_cache site (the delay line is gone → a fresh episode) must clear the
    // counter — guarded RELATIONALLY like the #1003 seams==frees check, and placed AFTER
    // free_async_cache so the #1003 `phase_anchor_ns = 0; free_async_cache(source);` adjacency stays
    // intact.
    let fac_counter_clears = src
        .matches("free_async_cache(source); source->genlock_acquire_bracket_ticks = 0;")
        .count();
    let frees = src.matches("free_async_cache(source);").count();
    assert_eq!(
        fac_counter_clears, frees,
        "{OBS_SOURCE}: #1161 — {frees} free_async_cache() site(s) but only {fac_counter_clears} \
         clear genlock_acquire_bracket_ticks afterwards; a stale bracket count could survive a \
         mid-episode delay-line drop and fail-open the next re-acquire early into a shallow lock."
    );
    // (d.2) genlock_backward_regime_end (zeroes the boundary → a fresh ACQUIRE) must clear it too —
    // scoped to that function, never a byte window (same discipline as the #1003 anchor check above).
    let regime_end_1161 = src
        .split("static void genlock_backward_regime_end(")
        .nth(1)
        .expect("#1161: genlock_backward_regime_end must exist");
    let regime_end_1161 = regime_end_1161
        .find("static ")
        .map_or(regime_end_1161, |i| &regime_end_1161[..i]);
    assert!(
        regime_end_1161.contains("source->genlock_acquire_bracket_ticks = 0;"),
        "{OBS_SOURCE}: #1161 — genlock_backward_regime_end no longer clears \
         genlock_acquire_bracket_ticks; a stale count from an interrupted re-acquire episode would \
         undercut the post-regime re-acquire's fail-open cap."
    );
}

/// #1161 — the ACQUIRE-bracket OBSERVABILITY marker (debug direction 3). The merged Stage-2 gate
/// was silent about WHY a raised pin did or did not deepen the FIFO — a live pin-rise re-acquire
/// left NO trace in the OBS log (the only line was the `#245` setter line), so a below-floor pin
/// (reserve < the arrival transport floor) that cannot move the frame was indistinguishable from a
/// working one. This one-per-ACQUIRE-tick marker exposes reserve_ms vs oldest_queued_age_ms and the
/// HOLD/ACQUIRE decision, so the NEXT live run self-diagnoses a below-floor pin. Emitted in the
/// N>=2 ACQUIRE branch only (a rare, bounded (re)acquire episode), never on the STEADY path. The
/// marker string is mutually-non-substring vs the other genlock log families
/// (`genlock-fifo audit` / `genlock-relock` / `genlock-ndi-*`) per the jitter-audit-parser rule.
#[test]
fn acquire_bracket_observability_marker_1161() {
    let src = vendor_file(OBS_SOURCE);
    assert!(
        src.contains("genlock-acquire-bracket '%s':"),
        "{OBS_SOURCE}: #1161 — the ACQUIRE-bracket observability marker (genlock-acquire-bracket) \
         is missing; a live pin-rise re-acquire leaves no trace of reserve vs oldest_queued_age, so \
         a below-floor (inert) pin cannot be diagnosed from the OBS log (debug direction 3)."
    );
    // The marker must report BOTH the decision and the two quantities that reveal a below-floor pin
    // (oldest_queued_age_ms >= reserve_ms with decision=ACQUIRE == the frame will not move).
    assert!(
        src.contains("oldest_queued_age_ms=") && src.contains("decision="),
        "{OBS_SOURCE}: #1161 — the genlock-acquire-bracket marker must carry oldest_queued_age_ms= \
         and decision= so a below-floor pin (age >= reserve, decision=ACQUIRE) is legible."
    );
    // It is a HOLD/ACQUIRE decision on the bracketing gate — both verdicts must be printable.
    assert!(
        src.contains("\"HOLD\"") && src.contains("\"ACQUIRE\""),
        "{OBS_SOURCE}: #1161 — the marker must distinguish decision=HOLD (queue deepening) from \
         decision=ACQUIRE (locking now — inert if oldest_age already >= reserve)."
    );
}
