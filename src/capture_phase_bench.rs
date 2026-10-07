//! Issue 1367 slice D2 — the two-clock bench of the capture phase tracker.
//!
//! A free-running camera (+-16 ppm against the per-second genlock grid, the Cam Link class)
//! captured through a jittery V4L2 timestamp and a jittery dequeue, driven frame by frame through
//! the REAL capture-loop pieces:
//!
//! - **raw** (today): the stamp is the floor of the raw capture time and the gate
//!   (`DecimationGate::poll`) decides on the poll wall clock;
//! - **tracked** (D2): `CapturePhase::stamp_frame` + `DecimationGate::note_stamp_slot`, exactly as
//!   `main.rs` wires them.
//!
//! Every emitted stamp (starvation repeats included, stamped like `main.rs` does) feeds the
//! receiver's own `genlock_grid::StampTrack`, the strih `stamp_dup=` / `stamp_gap=` counter. The
//! cambox side counts blind sheds and starvation repeats like the `(#889)` 5 s line.
//!
//! **Jitter model.** Each timestamp (and each dequeue) is a core Gaussian (8 us) plus, on 5 % of
//! frames, a wide one (100 us). With i.i.d. Gaussian noise the flips per crossing are
//! `~1.13 * sigma / drift-per-frame` — hundreds at the measured burst width — so the live counts
//! (35-55 shed + repeat pairs in 15-20 s on CAM4/CAM5/CAM6, 29.9.2026) need a narrow core with a
//! sparse wide tail: the tail sets the burst width, the core and the tail rate set the count.
//! These are the only calibrated knobs; re-derive them from new journal bursts, never to make a
//! change pass.
//!
//! Test-only, Linux-gated with `dupe_decimation`.

use crate::capture_phase::{slot_stamp_100ns, CapturePhase};
use crate::dupe_decimation::{DecimationGate, StampSlotAction};
use crate::genlock_grid::{per_second_floor, StampTrack, NS_PER_SECOND, UNITS_100NS_PER_SECOND};
use crate::genlock_pacing::{boundary_skip_count, starvation_repeat_timecode_100ns};
use crate::genlock_stamp::OFFSET_RESAMPLE_INTERVAL_FRAMES;

const FPS: u64 = 60;
const I60: u64 = NS_PER_SECOND / FPS;
const NOMINAL_60: f64 = NS_PER_SECOND as f64 / FPS as f64;
/// 2026-09-23 17:16:33 UTC — frame 0's realtime second (the #1355 tests' date).
const SEC_2309: u64 = 1_790_176_593;
/// CLOCK_MONOTONIC at frame 0's second (~3.5 days of uptime).
const MONO0: u64 = 300_000 * NS_PER_SECOND;

/// Capture -> dequeue-done latency (a Cam Link 1080p60 frame over USB 3 plus the wake-up).
const POLL_LATENCY_NS: f64 = 11_000_000.0;
const CORE_SIGMA_NS: f64 = 8_000.0;
const TAIL_SIGMA_NS: f64 = 100_000.0;
const TAIL_FRACTION: f64 = 0.05;

/// Deterministic noise (splitmix64); `gauss` is the Irwin-Hall sum of 12 uniforms.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }

    /// One draw of the bench's jitter model (ns), or of an i.i.d. Gaussian of `sigma_ns`.
    fn jitter(&mut self, sigma_ns: Option<f64>) -> f64 {
        let sigma = match sigma_ns {
            Some(s) => s,
            None if self.unit() < TAIL_FRACTION => TAIL_SIGMA_NS,
            None => CORE_SIGMA_NS,
        };
        sigma * self.gauss()
    }
}

/// One simulated camera run.
#[derive(Clone, Copy)]
struct Scenario {
    /// Camera frame-rate offset (positive = faster than the grid).
    ppm: f64,
    secs: f64,
    /// Frame 0's phase inside its grid slot (ns).
    start_phase_ns: f64,
    /// The device drops the frame at this second (its sequence number is never delivered).
    drop_at_s: Option<f64>,
    /// A CLOCK_REALTIME step (ns) at this second (a dantesync date step).
    realtime_step: Option<(f64, i64)>,
    /// A device re-open at this second: no frames for `gap_s`, then the sequence restarts at 0
    /// and the capture phase moves by `shift_ns`.
    reopen: Option<(f64, f64, f64)>,
    /// The DEVICE skips frames the sequence does not show: `(at_s, secs)` — no frames for `secs`,
    /// and the next one carries the next sequence number (a one-frame skip is `secs` = 1/60).
    hidden_skip: Option<(f64, f64)>,
    /// An i.i.d. Gaussian V4L2 timestamp jitter (ns) instead of the measured model.
    ts_sigma_ns: Option<f64>,
    seed: u64,
}

impl Scenario {
    fn new(ppm: f64, secs: f64) -> Self {
        Scenario {
            ppm,
            secs,
            start_phase_ns: NOMINAL_60 / 2.0,
            drop_at_s: None,
            realtime_step: None,
            reopen: None,
            hidden_skip: None,
            ts_sigma_ns: None,
            seed: 0x1367_d2be,
        }
    }
}

/// One delivered frame as the capture loop sees it.
struct Frame {
    t_s: f64,
    seq: u32,
    ts_mono: u64,
    poll_real: u64,
    poll_mono: u64,
    /// The mono -> real offset the loop holds for this frame (re-sampled every 100 frames).
    loop_offset_ns: i64,
}

fn frames(sc: &Scenario) -> Vec<Frame> {
    let period = NOMINAL_60 / (1.0 + sc.ppm * 1e-6);
    let off0 = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut ts_rng = Rng(sc.seed);
    let mut poll_rng = Rng(sc.seed ^ 0xD2D2_D2D2);
    let mut out = Vec::new();
    let mut loop_offset = off0;
    let mut since_sample = 0u64;
    let n = (sc.secs * FPS as f64) as u64;
    let mut reopened = false;
    let mut reopen_k = 0u64;
    let mut extra_ns = 0.0;
    let mut hidden = 0u64;
    for k in 0..n {
        let mut truth_rel = sc.start_phase_ns + k as f64 * period + extra_ns;
        if let Some((at, gap, shift)) = sc.reopen {
            if !reopened && truth_rel >= at * 1e9 {
                // The device goes away for `gap` seconds and comes back on a new phase.
                reopened = true;
                reopen_k = k;
                extra_ns = gap * 1e9 + shift;
                truth_rel += extra_ns;
            }
        }
        let t_s = truth_rel / 1e9;
        let ts_noise = ts_rng.jitter(sc.ts_sigma_ns);
        let poll_noise = poll_rng.jitter(None).abs();
        if sc.drop_at_s.is_some_and(|d| k == (d * FPS as f64) as u64) {
            continue;
        }
        if sc
            .hidden_skip
            .is_some_and(|(at, secs)| t_s >= at && t_s < at + secs)
        {
            hidden += 1;
            continue;
        }
        let truth = MONO0 as f64 + truth_rel;
        let off_true = off0
            + sc.realtime_step
                .filter(|&(at, _)| t_s >= at)
                .map_or(0, |(_, step)| step);
        if since_sample == 0 || since_sample >= OFFSET_RESAMPLE_INTERVAL_FRAMES {
            loop_offset = off_true;
            since_sample = 0;
        }
        since_sample += 1;
        let seq = if reopened {
            (k - reopen_k) as u32
        } else {
            (1000 + k - hidden) as u32
        };
        let poll_mono = (truth + POLL_LATENCY_NS + poll_noise).round() as u64;
        out.push(Frame {
            t_s,
            seq,
            ts_mono: (truth + ts_noise).round() as u64,
            poll_real: (poll_mono as i64 + off_true) as u64,
            poll_mono,
            loop_offset_ns: loop_offset,
        });
    }
    out
}

/// What one run produced.
#[derive(Default)]
struct Run {
    /// Cambox side: `(second, sheds, repeats)` per frame that shed or repeated.
    box_events: Vec<(f64, u64, u64)>,
    sheds: u64,
    repeats: u64,
    /// Receiver side: the seconds at which the strih `stamp_dup` / `stamp_gap` counters moved.
    rx_events: Vec<f64>,
    rx: StampTrack,
    /// Every emitted sender stamp, repeats included, in send order.
    stamps: Vec<i64>,
    /// `#707 SKIPPED` events (a boundary leap the capture loop logs).
    skip_events: u64,
    stamp_resyncs: u64,
    crossings: u64,
    reseeds: u64,
}

fn run(sc: &Scenario, tracked: bool) -> Run {
    let mut gate = DecimationGate::new();
    let mut phase = CapturePhase::new();
    let mut r = Run::default();
    for (k, f) in frames(sc).iter().enumerate() {
        let slot = if tracked {
            phase.stamp_frame(f.seq, f.ts_mono, f.loop_offset_ns, I60)
        } else {
            None
        };
        if let Some(s) = slot {
            gate.note_stamp_slot(s);
        }
        let prev = gate.next_boundary_ns();
        // Every frame is unique (a live camera) and came from an empty queue (the loop waited).
        let hash = (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let emit = gate.poll(f.poll_real, I60, hash, false, f.poll_mono, f.ts_mono);
        let repeats = gate.last_poll_starvation_repeats();
        if boundary_skip_count(prev, gate.next_boundary_ns(), I60)
            > gate.last_poll_intentional_extra_advance()
        {
            r.skip_events += 1;
        }
        if matches!(
            gate.last_stamp_action(),
            Some(StampSlotAction::Resync { .. })
        ) {
            r.stamp_resyncs += 1;
        }
        if k == 0 {
            // The gate's startup latch (never an emit, on either path) is not a shed.
            assert!(!emit);
            continue;
        }
        if !emit || repeats > 0 {
            r.box_events.push((f.t_s, u64::from(!emit), repeats));
            r.sheds += u64::from(!emit);
            r.repeats += repeats;
        }
        if !emit {
            continue;
        }
        let stamp = match slot {
            Some(s) => slot_stamp_100ns(s, I60, FPS),
            None => {
                let real_100ns = (f.ts_mono as i64 + f.loop_offset_ns) / 100;
                per_second_floor(real_100ns as u64, FPS, UNITS_100NS_PER_SECOND) as i64
            }
        };
        for j in (1..=repeats).rev() {
            let tc = starvation_repeat_timecode_100ns(stamp, j, FPS as i64);
            observe_rx(&mut r, tc, f.t_s);
        }
        observe_rx(&mut r, stamp, f.t_s);
        gate.note_emitted_stamp_100ns(stamp, I60);
    }
    r.crossings = phase.crossings();
    r.reseeds = phase.reseeds();
    r
}

fn observe_rx(r: &mut Run, stamp_100ns: i64, t_s: f64) {
    let before = r.rx.dups + r.rx.gaps;
    r.rx.observe(stamp_100ns as u64 * 100);
    if r.rx.dups + r.rx.gaps > before {
        r.rx_events.push(t_s);
    }
    r.stamps.push(stamp_100ns);
}

/// Group event seconds into bursts (events closer than 60 s): `(events, first, last)`.
fn bursts(times: &[f64]) -> Vec<(usize, f64, f64)> {
    let mut out: Vec<(usize, f64, f64)> = Vec::new();
    for &t in times {
        match out.last_mut() {
            Some(b) if t - b.2 < 60.0 => {
                b.0 += 1;
                b.2 = t;
            }
            _ => out.push((1, t, t)),
        }
    }
    out
}

/// The crossings of the TRUE capture phase over the grid in a run: slot steps that are not +1.
fn true_crossings(sc: &Scenario) -> usize {
    let period = NOMINAL_60 / (1.0 + sc.ppm * 1e-6);
    let n = (sc.secs * FPS as f64) as u64;
    let base = SEC_2309 * NS_PER_SECOND;
    let slot_of = |k: u64| {
        // Relative time in f64 (ns precision), the 2026 base added as an integer.
        let t = base + (sc.start_phase_ns + k as f64 * period).round() as u64;
        crate::genlock_grid::grid_floor_ns(t, I60)
    };
    (1..n)
        .filter(|&k| crate::genlock_grid::grid_steps_between(slot_of(k - 1), slot_of(k), I60) != 1)
        .count()
}

#[test]
fn raw_floor_reproduces_the_crossing_burst_and_the_tracker_gives_one_per_crossing_1367() {
    for ppm in [16.0f64, -16.0] {
        // ~2.2 cycles of the ~17 min crossing period: two stamp crossings for either sign.
        let sc = Scenario::new(ppm, 2_300.0);
        let expected = true_crossings(&sc);
        assert_eq!(
            expected, 2,
            "ppm {ppm}: the scenario must hold two crossings"
        );

        let raw = run(&sc, false);
        let raw_box: Vec<f64> = raw.box_events.iter().map(|e| e.0).collect();
        let box_bursts = bursts(&raw_box);
        let rx_bursts = bursts(&raw.rx_events);
        eprintln!(
            "ppm {ppm:+}: RAW cambox {} sheds + {} repeats in bursts {:?}; strih dup {} gap {} in \
             bursts {:?}",
            raw.sheds, raw.repeats, box_bursts, raw.rx.dups, raw.rx.gaps, rx_bursts
        );
        assert!(!box_bursts.is_empty() && !rx_bursts.is_empty());
        for b in box_bursts.iter().chain(rx_bursts.iter()) {
            assert!(
                b.0 >= 10,
                "ppm {ppm}: a raw crossing burst of only {} events",
                b.0
            );
            assert!(
                (5.0..45.0).contains(&(b.2 - b.1)),
                "ppm {ppm}: raw burst width {:.1} s (measured 15-20 s)",
                b.2 - b.1
            );
        }

        let tracked = run(&sc, true);
        eprintln!(
            "ppm {ppm:+}: TRACKED cambox {} sheds + {} repeats at {:?}; strih dup {} gap {}; \
             crossings {} reseeds {}",
            tracked.sheds,
            tracked.repeats,
            tracked.box_events,
            tracked.rx.dups,
            tracked.rx.gaps,
            tracked.crossings,
            tracked.reseeds
        );
        assert_eq!(tracked.crossings as usize, expected, "ppm {ppm}");
        assert_eq!(tracked.reseeds, 0, "ppm {ppm}");
        if ppm > 0.0 {
            assert_eq!(
                (tracked.sheds, tracked.repeats),
                (expected as u64, 0),
                "a fast camera drops exactly ONE duplicate slot per crossing"
            );
        } else {
            assert_eq!(
                (tracked.sheds, tracked.repeats),
                (0, expected as u64),
                "a slow camera fills exactly ONE missing slot per crossing"
            );
        }
        assert_eq!(
            (tracked.rx.dups, tracked.rx.gaps),
            (0, 0),
            "ppm {ppm}: the receiver sees a continuous stamp sequence"
        );
        assert_eq!(tracked.skip_events, 0);
    }
}

#[test]
fn a_dropped_usb_frame_gives_no_false_gap_1367() {
    let mut sc = Scenario::new(16.0, 120.0);
    sc.drop_at_s = Some(60.0);
    let t = run(&sc, true);
    assert_eq!(t.sheds, 0);
    assert_eq!(t.repeats, 1, "only the dropped frame's own slot is filled");
    assert_eq!(t.box_events.len(), 1);
    assert!((t.box_events[0].0 - 60.0).abs() < 0.1);
    assert_eq!((t.rx.dups, t.rx.gaps), (0, 0));
    assert_eq!((t.crossings, t.reseeds, t.skip_events), (0, 0, 0));
}

#[test]
fn a_700ms_date_step_gives_one_resync_1367() {
    for step in [700_000_000i64, -700_000_000] {
        let mut sc = Scenario::new(16.0, 120.0);
        sc.realtime_step = Some((60.0, step));
        let t = run(&sc, true);
        eprintln!(
            "step {step}: resyncs {} skip events {} sheds {} repeats {} rx {}/{}",
            t.stamp_resyncs, t.skip_events, t.sheds, t.repeats, t.rx.dups, t.rx.gaps
        );
        assert_eq!(t.stamp_resyncs, 1, "step {step}: one re-latch");
        assert_eq!(
            t.skip_events,
            u64::from(step > 0),
            "step {step}: a forward step logs one #707 SKIP, a backward one none"
        );
        assert_eq!(
            (t.sheds, t.repeats),
            (0, 0),
            "a 700 ms step (past the catch-up bound) is never filled"
        );
        assert_eq!(
            (t.crossings, t.reseeds),
            (0, 0),
            "the tracker (monotonic) never notices"
        );
    }
}

#[test]
fn a_device_reopen_gives_one_reseed_1367() {
    let mut sc = Scenario::new(16.0, 120.0);
    // 1.2 s without frames, then the sequence restarts at 0 and the phase moves 3.1 ms.
    sc.reopen = Some((60.0, 1.2, 3_100_000.0));
    let t = run(&sc, true);
    eprintln!(
        "reopen: reseeds {} skip events {} resyncs {} sheds {} repeats {} rx {}/{}",
        t.reseeds, t.skip_events, t.stamp_resyncs, t.sheds, t.repeats, t.rx.dups, t.rx.gaps
    );
    assert_eq!(t.reseeds, 1, "one re-open = one re-seed");
    assert_eq!(t.skip_events, 1, "the 1.2 s hole is one #707 SKIP");
    assert_eq!((t.sheds, t.repeats), (0, 0));
    assert_eq!((t.rx.dups, t.rx.gaps), (0, 0));
    assert_eq!(t.crossings, 0);
}

#[test]
fn same_timestamps_give_the_same_stamps_1367() {
    let mut sc = Scenario::new(-16.0, 700.0);
    sc.start_phase_ns = NOMINAL_60 - 2_000_000.0; // a crossing inside the run
    let a = run(&sc, true);
    let b = run(&sc, true);
    assert_eq!(a.stamps, b.stamps);
    assert_eq!(a.box_events, b.box_events);
    assert_eq!(a.crossings, 1);
}

/// Review round 1: a frame the DEVICE skipped (the sequence does not show it) must cost what it
/// costs today — one starvation repeat — never stamps from a stale prediction or a re-seed.
#[test]
fn a_frame_the_device_skipped_costs_one_repeat_like_today_1367() {
    let mut sc = Scenario::new(16.0, 120.0);
    sc.hidden_skip = Some((60.0, 1.0 / 60.0));
    let raw = run(&sc, false);
    let t = run(&sc, true);
    eprintln!(
        "hidden skip: raw sheds {} repeats {} rx {}/{}; tracked sheds {} repeats {} rx {}/{} \
         reseeds {} skips {}",
        raw.sheds,
        raw.repeats,
        raw.rx.dups,
        raw.rx.gaps,
        t.sheds,
        t.repeats,
        t.rx.dups,
        t.rx.gaps,
        t.reseeds,
        t.skip_events
    );
    assert_eq!(
        (raw.sheds, raw.repeats),
        (0, 1),
        "today: one starvation repeat"
    );
    assert_eq!((t.sheds, t.repeats), (0, 1));
    assert_eq!((t.rx.dups, t.rx.gaps), (0, 0));
    assert_eq!((t.reseeds, t.skip_events, t.crossings), (0, 0, 0));
}

/// Review round 1: a 1.5 s device pause the sequence does not show is one re-seed and one SKIP
/// line, and no stamp lands in the past of the stream.
#[test]
fn a_pause_the_sequence_does_not_show_gives_one_reseed_1367() {
    let mut sc = Scenario::new(16.0, 120.0);
    sc.hidden_skip = Some((60.0, 1.5));
    let t = run(&sc, true);
    eprintln!(
        "hidden pause: reseeds {} skips {} sheds {} repeats {} rx {}/{}",
        t.reseeds, t.skip_events, t.sheds, t.repeats, t.rx.dups, t.rx.gaps
    );
    assert_eq!(t.reseeds, 1);
    assert_eq!(t.skip_events, 1);
    assert_eq!((t.sheds, t.repeats), (0, 0));
    assert_eq!((t.rx.dups, t.rx.gaps), (0, 0));
    assert!(
        t.stamps.windows(2).all(|w| w[1] > w[0]),
        "every emitted stamp is later than the previous one"
    );
}

/// Review round 1: one event per crossing also at the design's stated raw jitter (0.3 ms i.i.d.
/// Gaussian on the V4L2 timestamp), not only at the calibrated burst model.
#[test]
fn the_tracker_gives_one_per_crossing_at_the_design_jitter_1367() {
    for ppm in [16.0f64, -16.0] {
        let mut sc = Scenario::new(ppm, 2_300.0);
        sc.ts_sigma_ns = Some(300_000.0);
        let t = run(&sc, true);
        eprintln!(
            "ppm {ppm:+} sigma 300 us: tracked sheds {} repeats {} rx {}/{} crossings {} reseeds {}",
            t.sheds, t.repeats, t.rx.dups, t.rx.gaps, t.crossings, t.reseeds
        );
        assert_eq!(t.crossings, 2, "ppm {ppm}");
        assert_eq!(t.sheds + t.repeats, 2, "ppm {ppm}: one event per crossing");
        assert_eq!((t.rx.dups, t.rx.gaps), (0, 0), "ppm {ppm}");
        assert_eq!(t.reseeds, 0, "ppm {ppm}");
    }
}

/// Issue 1372 part B — the nightly fleet date step on a stamp-driven 1:1 camera. A step that is a
/// whole number of slots (dantesync 1.16.0 rounds it to 200 ms) re-anchors the slot chooser by
/// exactly that many slots: no crossing, no shed, no repeat, and the stamp sequence with the step
/// taken out is one slot per frame — so a receiver that relabels its old-epoch frames by the same
/// step sees a continuous stream. The measured unquantized step (+1543.16 ms, 92.59 slots) moves the
/// content-to-slot phase by the fraction: one repeated or one skipped slot (reported).
///
/// The cambox stamps through a mono→real offset re-sampled every 100 frames, so its stamps switch to
/// the new epoch at that re-sample, up to 100 frames after its wall stepped (the step at 60.75 s
/// lands mid-cadence here). The lag is reported; the receiver-side cost is in the issue-1372 FIFO
/// relabel bench (`crate::genlock_fifo_relabel_bench`).
#[test]
fn a_whole_slot_date_step_re_anchors_by_whole_slots_with_no_crossing_1372() {
    let one_slot = |delta: i64| delta == 166_666 || delta == 166_667;
    for step in [1_600_000_000i64, -1_600_000_000, 200_000_000, 1_543_160_000] {
        let mut sc = Scenario::new(16.0, 120.0);
        sc.realtime_step = Some((60.75, step));
        let t = run(&sc, true);
        let step_100ns = step / 100;
        // the one stamp interval that carries the step (every other one is one slot)
        let jumps: Vec<usize> = t
            .stamps
            .windows(2)
            .enumerate()
            .filter(|(_, w)| (w[1] - w[0]).abs() > 5 * 166_667)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(jumps.len(), 1, "step {step}: exactly one stamp jump");
        let j = jumps[0];
        // the stamps with the step taken out from the jump on
        let relabelled: Vec<i64> = t
            .stamps
            .iter()
            .enumerate()
            .map(|(i, &s)| if i > j { s - step_100ns } else { s })
            .collect();
        let off_slot = relabelled
            .windows(2)
            .filter(|w| !one_slot(w[1] - w[0]))
            .count();
        // stamp j + 1 is frame j + 2 (frame 0 never emits): the epoch switch
        let period = NOMINAL_60 / (1.0 + sc.ppm * 1e-6);
        let switch_s = (sc.start_phase_ns + (j + 2) as f64 * period) / 1e9;
        eprintln!(
            "step {:+.2} ms: jump {:.2} slots at {:.3} s (stamp epoch lag {:.0} ms), relabelled \
             off-slot intervals {}, crossings {} reseeds {} sheds {} repeats {} resyncs {}",
            step as f64 / 1e6,
            (t.stamps[j + 1] - t.stamps[j]) as f64 / 166_666.67,
            switch_s,
            (switch_s - 60.75) * 1000.0,
            off_slot,
            t.crossings,
            t.reseeds,
            t.sheds,
            t.repeats,
            t.stamp_resyncs
        );
        assert_eq!(
            (t.crossings, t.reseeds, t.sheds, t.repeats),
            (0, 0, 0, 0),
            "step {step}: the date step is never a crossing, a re-seed, a shed or a repeat"
        );
        assert!(
            (0.0..=100.0 * period / 1e9 + 0.001).contains(&(switch_s - 60.75)),
            "step {step}: the stamps switch epoch at the next offset re-sample (<= 100 frames)"
        );
        if step % 200_000_000 == 0 {
            assert_eq!(
                off_slot, 0,
                "issue 1372: a whole-slot step must re-anchor by whole slots: with the step taken \
                 out the stamps are one slot per frame (step {step})"
            );
        } else {
            // exactly one: the fraction shows (so the measure above can see an off-slot interval,
            // and the quantized steps' 0 is not vacuous)
            assert_eq!(
                off_slot, 1,
                "the unquantized step moves the phase by a fraction: exactly one off-slot interval"
            );
        }
    }
}
