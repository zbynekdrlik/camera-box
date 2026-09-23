//! #1355 — a Tier-0 two-clock-domain N==1 genlock bench.
//!
//! This is a PURE, default-features simulation of the stream box's `NDI 2ME PGM` release path
//! under two independent clock domains — it renders/decodes nothing, on purpose (the
//! `src/asrc_bench.rs` two-clock-domain precedent, `.claude/rules/asrc-bench-harness.md`). Its job
//! is to REPRODUCE the measured A/V-level walk offline, before any rig time, and to PROVE the
//! N==1 release-phase hysteresis fix removes it — so a fix never ships on an unverified model
//! (the attempt-1 mistake, which had no bench and cost two live E2E cycles).
//!
//! ## The two clocks (measured root cause, issue 1355 comments 5784916723 / 5787592724)
//!
//! The video release deadline keys on the WALL clock (`genlock_wall_now_ns` =
//! `GetSystemTimePreciseAsFileTime`, DanteSync-disciplined), while the render tick + audio ride
//! the free-running monotonic/QPC clock. On the stream box the two drift ~+13 ppm apart. The
//! boundary-locked N==1 conveyor advances exactly one stamp per render tick, so with the wall
//! (arrivals + deadline) running faster than the render tick, the FIFO gains one frame of on-air
//! age every ~40 min. The only N==1 excess-shed mechanism is the #859 depth drain
//! ([`should_drain_one`]) whose fixed 2-frame hysteresis ([`DRAIN_HYSTERESIS_FRAMES`]) a 1-2 frame
//! phase error sits inside by construction → the on-air age (and thus the on-air A/V offset) saws
//! over a ~2-frame band on a ~40-min period. The pin never "settles".
//!
//! ## Faithfulness — it REUSES the authority, never a copy
//!
//! [`simulate`] drives the CURRENT release rule out of [`crate::genlock_backlog`]'s own pure
//! decisions ([`phase_pinned_deadline`], [`phase_pinned_is_due`], [`should_drain_one`],
//! [`backlog_relock_threshold`]) and the fix out of [`n1_release_phase_step`]. A change to any of
//! those decisions changes the bench, so the bench cannot drift away from what ships.
//!
//! ## What this single-source model DOES and does NOT reproduce (be honest — issue 1355)
//!
//! It faithfully reproduces **symptom 1**, the A/V-level walk / ~2-frame saw
//! ([`current_release_rule`] at 13 ppm reaches `band ≈ 2.07` frames; at 0 ppm `≈ 0.35`), and shows
//! the fix holds it within one frame.
//!
//! It does NOT reproduce **symptom 2**, the balanced dup+skip bursts (comment 5787592724), and
//! that is STRUCTURAL, not a tuning gap: at the deep 963 ms pin the queue is ~29 frames, so the
//! oldest frame is always aged far past the boundary and the boundary-locked STEADY path can never
//! HOLD — a drain shed drops `array[0]` and presents `array[1]`, re-anchoring the boundary to
//! `array[1].ts + interval`, and the next head is still matured, so no copy is ever produced. With
//! the #401 boundary-lock + #940 phase-pin in place the deep N==1 conveyor cannot churn. The live
//! bursts therefore originate OUTSIDE a single clean-30fps-sender model — most plausibly the
//! upstream strih→stream `2ME PGM` arrival pattern (the strih program is itself a genlocked
//! composite, not a clean sender) — and are validated on the rig, not here. The bench PINS this
//! finding (the `current_rule_shows_no_dup_skip_bursts_single_source` test) so a future edit
//! cannot silently start claiming the single-source model reproduces them.

use crate::genlock_backlog::{
    backlog_relock_threshold, drain_target_frames, n1_release_phase_step, phase_pinned_deadline,
    phase_pinned_is_due, should_drain_one,
};
use std::collections::VecDeque;

/// One frame interval at 30 fps, in ns (the canvas / render-tick interval).
pub const INTERVAL_30_NS: u64 = 33_333_333;
/// The sender's 100 ns-truncated 30 fps stamp grid — a 33 ns/frame beat against the receiver
/// interval, exactly as the live senders stamp (`tests/genlock_relock_selection_parity.rs`).
pub const SENDER_GRID_30_NS: u64 = 33_333_300;

/// Which release rule the bench drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseRule {
    /// The CURRENT shipped N==1 path: boundary-locked STEADY conveyor + the #859 depth drain.
    Current,
    /// Step B: the CURRENT path plus the N==1 release-phase hysteresis ([`n1_release_phase_step`]),
    /// which preempts the coarse 2-frame-hysteresis drain with one clean whole-frame step per
    /// crossing.
    N1PhaseStep,
}

/// The bench configuration — one N==1 source under two drifting clocks.
#[derive(Clone, Copy, Debug)]
pub struct BenchConfig {
    /// Canvas / render-tick interval (ns). Use [`INTERVAL_30_NS`].
    pub interval_ns: u64,
    /// Sender stamp grid (ns). Use [`SENDER_GRID_30_NS`] for the live 33 ns/frame beat, or set
    /// equal to `interval_ns` to isolate the pure drift.
    pub sender_grid_ns: u64,
    /// Configured genlock latency / pin, in ms (the stream box's `NDI 2ME PGM` runs ~963).
    pub latency_ms: u32,
    /// Source frame rate numerator / denominator (30 / 1 for the 30-into-30 `2ME PGM` hop).
    pub fps_num: u32,
    pub fps_den: u32,
    /// Wall-vs-render drift, ppm. POSITIVE = the wall clock (arrivals + deadline) runs FASTER than
    /// the render tick (the live stream-box sign; +13 measured).
    pub drift_ppm: f64,
    /// Per-frame arrival jitter amplitude (ns): a frame's arrival wall-time is
    /// `capture + skew ± arrival_jitter`, so the FIFO front depth oscillates ±1 — the signal the
    /// #859 drain and the depth toggle 31↔32 (comment 5786754812) read.
    pub arrival_jitter_ns: i64,
    /// Transport skew (ns) — a fixed arrival delay. Inert at deep latency; kept for realism.
    pub skew_ns: u64,
    /// Render-tick reading slew amplitude (ns) — noise in the wall instant the render tick
    /// observes (the ±2 ms the #940 phase-pin hysteresis was sized to absorb).
    pub render_jitter_ns: i64,
    /// Total render ticks to simulate. ~30 * seconds; 400_000 ≈ 3.7 h.
    pub n_ticks: u64,
    /// Ticks to run before recording metrics (lets the FIFO fill to its steady depth).
    pub warmup_ticks: u64,
    /// The release rule under test.
    pub rule: ReleaseRule,
}

impl BenchConfig {
    /// The live stream-box `NDI 2ME PGM` case: 963 ms pin, 30-into-30, ±5 ms arrival jitter, the
    /// 33 ns/frame sender beat, ~3.7 h, CURRENT rule.
    pub fn stream_2me_pgm() -> Self {
        BenchConfig {
            interval_ns: INTERVAL_30_NS,
            sender_grid_ns: SENDER_GRID_30_NS,
            latency_ms: 963,
            fps_num: 30,
            fps_den: 1,
            drift_ppm: 13.0,
            arrival_jitter_ns: 5_000_000,
            skew_ns: 8_000_000,
            render_jitter_ns: 0,
            n_ticks: 400_000,
            warmup_ticks: 3_000,
            rule: ReleaseRule::Current,
        }
    }

    /// Same case with the N==1 phase-step fix engaged.
    pub fn with_fix(mut self) -> Self {
        self.rule = ReleaseRule::N1PhaseStep;
        self
    }
}

/// The bench outputs the design asks for: the presented-age trace (min/max/band) and the
/// dup / skip / drop counts, plus burst-clustering evidence for the honest symptom-2 finding.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BenchResult {
    /// `max_presented_age − min_presented_age`, in whole frames — the A/V-level walk band.
    pub band_frames: f64,
    /// Minimum / maximum presented on-air age observed (ns), after warmup.
    pub min_presented_age_ns: u64,
    pub max_presented_age_ns: u64,
    /// Duplicated program frames (a HOLD re-presents the last frame).
    pub dups: u64,
    /// Skipped program frames (a drain / phase-step / relock drop retires a captured frame).
    pub skips: u64,
    /// Total captured frames dropped without going to air (== `skips` here; kept distinct for
    /// clarity against the C `genlock_dropped_due`).
    pub drops: u64,
    /// The most dup+skip events in any 300-tick (~10 s) window — the burst-clustering signal.
    pub max_events_per_window: u64,
    /// How many 300-tick windows carried >= 4 dup+skip events (a "burst").
    pub bursty_windows: u64,
}

/// A deterministic splitmix64, so every run is byte-reproducible.
struct SplitMix64(u64);
impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[-amp, amp]`.
    fn jitter(&mut self, amp_ns: i64) -> i64 {
        if amp_ns == 0 {
            return 0;
        }
        let span = (2 * amp_ns + 1) as u64;
        (self.next_u64() % span) as i64 - amp_ns
    }
}

/// The wall-clock arrival time of capture frame `k`, deterministic in `k` alone (so ingest is
/// independent of tick order).
fn arrival_of(k: u64, base_ns: u64, cfg: &BenchConfig) -> u64 {
    let mut r =
        SplitMix64(0x1122_3344_5566_7788u64.wrapping_add(k.wrapping_mul(0x1_0000_0001_00B3)));
    let j = r.jitter(cfg.arrival_jitter_ns);
    let cap = base_ns + k.wrapping_mul(cfg.sender_grid_ns);
    (cap as i64 + cfg.skew_ns as i64 + j).max(0) as u64
}

/// Run the whole simulation and return the metrics.
///
/// The release model is the CURRENT shipped N==1 path: a boundary-locked STEADY conveyor
/// (`array[0].ts <= boundary` → present the oldest, re-anchor `boundary = presented + interval`),
/// pre-empted by the #859 depth drain and the backlog relock, reusing
/// [`crate::genlock_backlog`]'s pure decisions verbatim. Under [`ReleaseRule::N1PhaseStep`] the
/// STEADY tick additionally consults [`n1_release_phase_step`] and sheds one clean whole-frame step
/// per crossing, sharing the drain's throttle.
pub fn simulate(cfg: &BenchConfig) -> BenchResult {
    let base_ns: u64 = 10_000_000_000_000;
    let wall0_ns: u64 = base_ns + 5_000_000_000;
    let reserve_ns = cfg.latency_ms as u64 * 1_000_000;
    let n_mult: u32 = 1; // N==1 source

    let mut render_rng = SplitMix64(0xDEAD_BEEF_1234_5678);
    let mut queue: VecDeque<u64> = VecDeque::new();
    let mut next_k: u64 = 0;
    let mut boundary_ns: u64 = 0;
    let mut ticks_since_drain: u64 = 0;
    let mut last_presented: Option<u64> = None;

    let mut dups: u64 = 0;
    let mut skips: u64 = 0;
    let mut drops: u64 = 0;
    let mut min_age = u64::MAX;
    let mut max_age = 0u64;
    let mut events: Vec<u64> = Vec::new();

    for m in 0..cfg.n_ticks {
        // Render tick m fires on the QPC/monotonic clock; the wall clock runs `drift_ppm` faster.
        let wall_true = wall0_ns
            + ((m as f64) * (cfg.interval_ns as f64) * (1.0 + cfg.drift_ppm * 1e-6)).round() as u64;

        // Ingest every frame that has arrived (in-order, a single NDI source delivers monotonically).
        while arrival_of(next_k, base_ns, cfg) <= wall_true {
            queue.push_back(base_ns + next_k * cfg.sender_grid_ns);
            next_k += 1;
            if next_k > m + 10_000 {
                break; // safety valve; unreachable in practice
            }
        }

        let recording = m >= cfg.warmup_ticks;
        if queue.is_empty() {
            if last_presented.is_some() && recording {
                dups += 1;
                events.push(m);
            }
            continue;
        }

        let wall_read = (wall_true as i64 + render_rng.jitter(cfg.render_jitter_ns)).max(0) as u64;
        let present_ts =
            phase_pinned_deadline(wall_read.saturating_sub(reserve_ns), cfg.interval_ns);
        let due = queue
            .iter()
            .take_while(|&&ts| phase_pinned_is_due(ts, present_ts))
            .count() as u64;
        let depth = queue.len() as u64;
        let backlog_thr =
            backlog_relock_threshold(cfg.latency_ms, cfg.fps_num, cfg.fps_den, n_mult);
        let head = *queue.front().unwrap();

        // Classify the N==1 tick exactly as `genlock_release_tick` does.
        if boundary_ns == 0 {
            // ACQUIRE: the first due frame locks the cadence.
            if due == 0 {
                if recording {
                    dups += 1;
                    events.push(m);
                }
                continue;
            }
            let pres = queue.pop_front().unwrap();
            boundary_ns = pres + cfg.interval_ns;
            ticks_since_drain = 0;
            last_presented = Some(pres);
            record_present(wall_true, pres, recording, &mut min_age, &mut max_age);
            continue;
        }

        if depth > backlog_thr && due > 0 {
            // BACKLOG relock: shed the overshoot down to the drain target, present the next.
            let target = drain_target_frames(cfg.latency_ms, cfg.fps_num, cfg.fps_den);
            let mut to_drop = depth.saturating_sub(target);
            while to_drop > 0 && queue.len() > 1 {
                queue.pop_front();
                if recording {
                    drops += 1;
                    skips += 1;
                    events.push(m);
                }
                to_drop -= 1;
            }
            let pres = queue.pop_front().unwrap();
            boundary_ns = pres + cfg.interval_ns;
            last_presented = Some(pres);
            record_present(wall_true, pres, recording, &mut min_age, &mut max_age);
            continue;
        }

        if head <= boundary_ns {
            // STEADY (strict FIFO): present the oldest matured frame.
            //
            // The #859 depth drain runs FIRST (drops array[0] if in genuine backlog), then — under
            // the fix — the N==1 phase step. Both share `ticks_since_drain`, so at most one extra
            // frame leaves the queue this tick and the drain block stays byte-identical.
            if should_drain_one(
                depth,
                cfg.latency_ms,
                cfg.fps_num,
                cfg.fps_den,
                ticks_since_drain,
            ) && queue.len() > 1
            {
                queue.pop_front();
                if recording {
                    drops += 1;
                    skips += 1;
                    events.push(m);
                }
                ticks_since_drain = 0;
            } else {
                ticks_since_drain += 1;
            }
            if cfg.rule == ReleaseRule::N1PhaseStep {
                let shed = n1_release_phase_step(
                    wall_true,
                    boundary_ns,
                    cfg.latency_ms,
                    cfg.interval_ns,
                    n_mult,
                    ticks_since_drain,
                );
                if shed && queue.len() > 1 {
                    queue.pop_front();
                    if recording {
                        drops += 1;
                        skips += 1;
                        events.push(m);
                    }
                    ticks_since_drain = 0;
                }
            }
            let pres = queue.pop_front().unwrap();
            if let Some(prev) = last_presented {
                if pres == prev && recording {
                    dups += 1;
                    events.push(m);
                }
            }
            boundary_ns = pres + cfg.interval_ns;
            last_presented = Some(pres);
            record_present(wall_true, pres, recording, &mut min_age, &mut max_age);
            continue;
        }

        if present_ts >= head {
            // GAP RESYNC: nothing matured by the boundary, but the head aged past the reserve.
            let pres = queue.pop_front().unwrap();
            boundary_ns = pres + cfg.interval_ns;
            last_presented = Some(pres);
            record_present(wall_true, pres, recording, &mut min_age, &mut max_age);
            continue;
        }

        // HOLD: the boundary's frame has not arrived / not aged — repeat the last frame.
        if recording {
            dups += 1;
            events.push(m);
        }
    }

    let (max_events_per_window, bursty_windows) = window_bursts(&events, 300, 4);
    let band_frames = if max_age >= min_age && min_age != u64::MAX {
        (max_age - min_age) as f64 / cfg.interval_ns as f64
    } else {
        0.0
    };

    BenchResult {
        band_frames,
        min_presented_age_ns: if min_age == u64::MAX { 0 } else { min_age },
        max_presented_age_ns: max_age,
        dups,
        skips,
        drops,
        max_events_per_window,
        bursty_windows,
    }
}

fn record_present(
    wall_true: u64,
    presented_ts: u64,
    recording: bool,
    min_age: &mut u64,
    max_age: &mut u64,
) {
    if !recording {
        return;
    }
    let age = wall_true.saturating_sub(presented_ts);
    if age < *min_age {
        *min_age = age;
    }
    if age > *max_age {
        *max_age = age;
    }
}

/// The most events in any `win`-tick window, and how many windows carried >= `burst_min` events.
fn window_bursts(events: &[u64], win: u64, burst_min: u64) -> (u64, u64) {
    if events.is_empty() {
        return (0, 0);
    }
    let last = *events.last().unwrap();
    let mut max_win = 0u64;
    let mut bursty = 0u64;
    let mut w = events[0];
    while w <= last {
        let c = events.iter().filter(|&&e| e >= w && e < w + win).count() as u64;
        if c > max_win {
            max_win = c;
        }
        if c >= burst_min {
            bursty += 1;
        }
        w += win;
    }
    (max_win, bursty)
}

/// Convenience: the CURRENT release rule over `cfg`.
pub fn current_release_rule(cfg: &BenchConfig) -> BenchResult {
    let mut c = *cfg;
    c.rule = ReleaseRule::Current;
    simulate(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Step A: reproduce symptom 1 (the A/V-level walk) with the CURRENT rule ----

    #[test]
    fn current_release_rule_reproduces_the_av_walk_at_13ppm() {
        // The design's Step A RED criterion: at 13 ppm + arrival jitter the CURRENT rule walks the
        // presented on-air age over a band of AT LEAST 2 frames — the #859 drain's 2-frame
        // hysteresis, exactly as measured live (comment 5784916723).
        let r = current_release_rule(&BenchConfig::stream_2me_pgm());
        assert!(
            r.band_frames >= 2.0,
            "the CURRENT rule must reproduce the >= 2-frame A/V walk at 13 ppm, got band {:.3} \
             frames (min {} ns, max {} ns)",
            r.band_frames,
            r.min_presented_age_ns,
            r.max_presented_age_ns
        );
    }

    #[test]
    fn current_rule_band_is_flat_at_zero_drift() {
        // Anti-tautology / control: with the two clocks in step there is no walk. This is what
        // makes the 13 ppm reproduction above evidence of the DRIFT, not of the model.
        let cfg = BenchConfig {
            drift_ppm: 0.0,
            ..BenchConfig::stream_2me_pgm()
        };
        let r = current_release_rule(&cfg);
        assert!(
            r.band_frames < 1.0,
            "at 0 ppm the band must stay well under one frame, got {:.3}",
            r.band_frames
        );
    }

    // ---- The honest symptom-2 finding, PINNED so it cannot silently regress ----

    #[test]
    fn current_rule_shows_no_dup_skip_bursts_single_source() {
        // Symptom 2 (the balanced dup+skip bursts) does NOT arise in this faithful single-source
        // model — the deep boundary-locked N==1 conveyor cannot HOLD (see the module doc). This
        // test PINS that finding: the CURRENT rule produces zero dups and only a handful of
        // spread-out drain sheds, never a burst. If a future edit makes the single-source model
        // start churning, this fails and forces the change to be justified honestly.
        let r = current_release_rule(&BenchConfig::stream_2me_pgm());
        assert_eq!(
            r.dups, 0,
            "the deep N==1 conveyor never holds → zero copies"
        );
        assert!(
            r.max_events_per_window < 4 && r.bursty_windows == 0,
            "no dup+skip bursts in the single-source model, got max {}/window, {} bursty windows",
            r.max_events_per_window,
            r.bursty_windows
        );
    }

    // ---- Step B: the N==1 phase-step fix holds the band within one frame (RED until [green]) ----

    #[test]
    fn n1_phase_step_holds_the_band_within_one_frame() {
        // Step B GREEN criterion: with the fix engaged the presented-age band collapses to <= 1
        // frame. RED against the [red] stub (fix == current → band ~2 frames > 1), GREEN once
        // `n1_release_phase_step` is implemented.
        let r = simulate(&BenchConfig::stream_2me_pgm().with_fix());
        assert!(
            r.band_frames <= 1.05,
            "the N==1 phase-step must hold the band within one frame, got {:.3} frames",
            r.band_frames
        );
    }

    #[test]
    fn the_fix_introduces_no_bursts() {
        // The fix must remove the walk with CLEAN single steps — never trade the walk for churn.
        let r = simulate(&BenchConfig::stream_2me_pgm().with_fix());
        assert_eq!(r.dups, 0, "the fix must not introduce copies");
        assert!(
            r.max_events_per_window < 4 && r.bursty_windows == 0,
            "the fix must shed cleanly (no bursts), got max {}/window, {} bursty windows",
            r.max_events_per_window,
            r.bursty_windows
        );
    }

    #[test]
    fn the_fix_is_byte_identical_at_zero_drift() {
        // No behaviour change when the clocks are in step: the phase-step threshold
        // (reserve + interval − budget) is never reached at 0 ppm, so CURRENT and the fix produce
        // identical traces. (Holds in BOTH the [red] stub and the [green] body — a real invariant.)
        let cfg = BenchConfig {
            drift_ppm: 0.0,
            ..BenchConfig::stream_2me_pgm()
        };
        let cur = current_release_rule(&cfg);
        let fixed = simulate(&BenchConfig {
            rule: ReleaseRule::N1PhaseStep,
            ..cfg
        });
        assert_eq!(
            cur, fixed,
            "the fix must be byte-identical to the CURRENT rule at 0 ppm drift"
        );
    }

    // ---- Unit checks on the pure decision itself ----

    #[test]
    fn phase_step_is_n1_only_and_guards_degenerates() {
        // N>=2 stays with should_converge_phase (unchanged, #1049); a degenerate interval or an
        // unlocked boundary never fires.
        let interval = INTERVAL_30_NS;
        let reserve_ms = 963u32;
        // A deep N==1 walk (on-air age well past reserve + a frame) with the throttle met.
        let wall = 10_000_000_000_000u64;
        let boundary = wall.saturating_sub(reserve_ms as u64 * 1_000_000 + 2 * interval);
        assert!(
            !n1_release_phase_step(wall, boundary, reserve_ms, interval, 2, 100),
            "N>=2 must be left to should_converge_phase"
        );
        assert!(
            !n1_release_phase_step(wall, boundary, reserve_ms, 0, 1, 100),
            "a degenerate interval must never fire"
        );
        assert!(
            !n1_release_phase_step(wall, 0, reserve_ms, interval, 1, 100),
            "an unlocked boundary must never fire"
        );
        assert!(
            !n1_release_phase_step(wall, boundary, reserve_ms, interval, 1, 29),
            "the throttle (>= DRAIN_MIN_TICK_INTERVAL) must gate the shed"
        );
    }

    #[test]
    fn n1_release_phase_step_fires_for_a_deep_n1_walk() {
        // The core GREEN behaviour, as a direct unit assertion (RED against the stub): once the
        // on-air age has walked a full frame past the configured hold, an N==1 source with the
        // throttle met SHEDS.
        let interval = INTERVAL_30_NS;
        let reserve_ms = 963u32;
        let reserve_ns = reserve_ms as u64 * 1_000_000;
        let wall = 10_000_000_000_000u64;
        // age = wall - boundary set one full frame above the configured hold.
        let boundary = wall - (reserve_ns + interval);
        assert!(
            n1_release_phase_step(wall, boundary, reserve_ms, interval, 1, 100),
            "a deep N==1 source walked a full frame past the hold must shed with the throttle met"
        );
        // Held AT the configured latency (no walk) → inert.
        let boundary_held = wall - reserve_ns;
        assert!(
            !n1_release_phase_step(wall, boundary_held, reserve_ms, interval, 1, 100),
            "a source held at the configured latency must NOT shed"
        );
    }
}
