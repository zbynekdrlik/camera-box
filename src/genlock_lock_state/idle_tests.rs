//! Issue 1302 — the unit cases of the fast first idle classification
//! ([`super::input_idle_class`]). A `#[path]` child of `genlock_lock_state::phase_events`, in its own
//! file to keep that module's size down.
//!
//! The widget feeds the classifier one 1 Hz tick at a time from a per-input ring of
//! (monotonic ms, cumulative frames received) pruned to [`GENLOCK_IDLE_WINDOW_MS`]: a counter that
//! goes backward clears the ring and the class (a reconnect), and an input that leaves the scan is
//! forgotten (its return is a first sight). [`Ring`] is that mechanic in a few lines, so each case
//! below reads like the input's real life; the shipped C++ ring is replayed against the same rule
//! in `tests/genlock_idle_class_1302.rs`.

use super::*;
use std::collections::VecDeque;

/// The widget's per-input ring, as `genlock_idle_classify_tick` keeps it.
#[derive(Default)]
struct Ring {
    samples: VecDeque<(i64, u64)>,
    class: Option<InputIdleClass>,
}

impl Ring {
    fn tick(&mut self, now_ms: i64, frames: u64) -> InputIdleClass {
        if self.samples.back().is_some_and(|&(_, f)| frames < f) {
            self.samples.clear();
            self.class = None;
        }
        self.samples.push_back((now_ms, frames));
        while self.samples.len() > 1
            && now_ms - self.samples.front().expect("non-empty").0 > GENLOCK_IDLE_WINDOW_MS
        {
            self.samples.pop_front();
        }
        let (t0, f0) = *self.samples.front().expect("non-empty");
        let (t1, f1) = *self.samples.back().expect("non-empty");
        let class = input_idle_class(
            t1 - t0,
            f1 - f0,
            self.class.unwrap_or(InputIdleClass::Unclassified),
        );
        self.class = Some(class);
        class
    }
}

/// The class on each 1 Hz tick of an input first seen at second 0 whose cumulative frame counter
/// reads `frames(second)`.
fn life(seconds: i64, frames: impl Fn(i64) -> u64) -> Vec<InputIdleClass> {
    let mut ring = Ring::default();
    (0..seconds)
        .map(|s| ring.tick(s * 1000, frames(s)))
        .collect()
}

use InputIdleClass::{Idle, Live, Unclassified};

#[test]
fn the_class_codes_match_the_c_enum_1302() {
    assert_eq!(Unclassified.code(), 0);
    assert_eq!(Live.code(), 1);
    assert_eq!(Idle.code(), 2);
    assert_eq!(GENLOCK_IDLE_FULL_SPAN_MS, 54_000);
}

#[test]
fn a_keep_alive_reconnect_never_contributes_1302() {
    // a SongPlayer playlist input: one keep-alive frame per ~11 s, right after it (re)connected
    let classes = life(80, |s| 1000 + (s / 11) as u64);
    for (s, c) in classes.iter().enumerate() {
        assert_ne!(
            *c, Live,
            "second {s}: a keep-alive input must never contribute"
        );
        let want = if (s as i64) * 1000 >= GENLOCK_IDLE_FULL_SPAN_MS {
            Idle
        } else {
            Unclassified
        };
        assert_eq!(*c, want, "second {s}");
    }
}

#[test]
fn a_live_reconnect_contributes_after_5_s_1302() {
    // a live 60 fps source right after it (re)connected (its lifetime counter is far from 0)
    let classes = life(70, |s| 500_000 + 60 * s as u64);
    for (s, c) in classes.iter().enumerate() {
        let want = if s < 5 { Unclassified } else { Live };
        assert_eq!(*c, want, "second {s}");
    }
}

#[test]
fn obs_start_classifies_a_live_input_in_5_s_and_a_keep_alive_one_never_live_1302() {
    // every input is a first sight at the start: 60, 30 and 23.98 fps cameras, a keep-alive input
    for fps in [60u64, 30] {
        let classes = life(70, |s| fps * s as u64);
        assert_eq!(&classes[..5], &[Unclassified; 5], "{fps} fps");
        assert!(classes[5..].iter().all(|c| *c == Live), "{fps} fps");
    }
    let classes = life(70, |s| (s as u64 * 2398) / 100);
    assert_eq!(&classes[..5], &[Unclassified; 5], "23.98 fps");
    assert!(classes[5..].iter().all(|c| *c == Live), "23.98 fps");
    let keep_alive = life(70, |s| (s / 11) as u64);
    assert!(keep_alive.iter().all(|c| *c != Live));
    assert_eq!(keep_alive[69], Idle);
}

#[test]
fn a_counter_reset_reclassifies_from_scratch_1302() {
    // a live input for 70 s, then its source is recreated: the received counter restarts at 0 with
    // no disconnect in between -> UNCLASSIFIED for 5 s, then LIVE again
    let mut ring = Ring::default();
    for s in 0..70 {
        ring.tick(s * 1000, 60 * s as u64);
    }
    assert_eq!(ring.class, Some(Live));
    let after: Vec<_> = (0..10)
        .map(|k| ring.tick((70 + k) * 1000, 60 * k as u64))
        .collect();
    assert_eq!(&after[..5], &[Unclassified; 5]);
    assert_eq!(&after[5..], &[Live; 5]);
    // and a keep-alive input whose counter resets never turns LIVE in its fresh ring
    let mut ring = Ring::default();
    for s in 0..70 {
        ring.tick(s * 1000, 100 + (s / 11) as u64);
    }
    assert_eq!(ring.class, Some(Idle));
    let after: Vec<_> = (0..20)
        .map(|k| ring.tick((70 + k) * 1000, (k / 11) as u64))
        .collect();
    assert!(after.iter().all(|c| *c == Unclassified), "{after:?}");
}

#[test]
fn a_slow_1_to_5_fps_source_turns_live_once_60_frames_arrived_1302() {
    // 5 fps: 60 frames after 12 s; 2 fps: after 30 s. Never LIVE before, never IDLE on the way.
    for (fps, live_at) in [(5u64, 12usize), (2, 30)] {
        let classes = life(70, |s| fps * s as u64);
        assert!(
            classes[..live_at].iter().all(|c| *c == Unclassified),
            "{fps} fps: {classes:?}"
        );
        assert!(
            classes[live_at..].iter().all(|c| *c == Live),
            "{fps} fps: {classes:?}"
        );
    }
    // 1 fps never reaches 60 frames before the full window: the full-window rule decides, as before
    // issue 1302 (IDLE while the window holds fewer than 60 frames, LIVE from 60 on)
    let classes = life(70, |s| s as u64);
    assert!(classes[..54].iter().all(|c| *c == Unclassified));
    assert!(classes[54..60].iter().all(|c| *c == Idle), "{classes:?}");
    assert!(classes[60..].iter().all(|c| *c == Live), "{classes:?}");
}

#[test]
fn the_rule_at_its_boundaries_1302() {
    // the fast stage: both the span and the frame floor are inclusive
    assert_eq!(input_idle_class(4_999, 10_000, Unclassified), Unclassified);
    assert_eq!(input_idle_class(5_000, 59, Unclassified), Unclassified);
    assert_eq!(input_idle_class(5_000, 60, Unclassified), Live);
    assert_eq!(input_idle_class(53_999, 60, Unclassified), Live);
    // the fast stage never says IDLE
    assert_eq!(input_idle_class(53_999, 0, Unclassified), Unclassified);
    // the full window decides on its own, whatever the previous class
    for prev in [Unclassified, Live, Idle] {
        assert_eq!(input_idle_class(54_000, 59, prev), Idle);
        assert_eq!(input_idle_class(54_000, 60, prev), Live);
        assert_eq!(input_idle_class(i64::MAX, u64::MAX, prev), Live);
    }
    // a decided class holds while the ring spans less than the full window (a long widget stall
    // pruned it): a LIVE input is never demoted on a short ring, an IDLE one stays IDLE until it
    // delivers a live rate -- then the fast rule turns it LIVE like an UNCLASSIFIED one (a keep-alive
    // input can never meet it)
    assert_eq!(input_idle_class(0, 0, Live), Live);
    assert_eq!(input_idle_class(5_000, 0, Live), Live);
    assert_eq!(input_idle_class(10_000, 59, Idle), Idle);
    assert_eq!(input_idle_class(4_999, 600, Idle), Idle);
    assert_eq!(input_idle_class(5_000, 60, Idle), Live);
    assert_eq!(input_idle_class(10_000, 600, Idle), Live);
    assert_eq!(input_idle_class(-1, 1_000, Unclassified), Unclassified);
    assert_eq!(input_idle_class(i64::MIN, 0, Idle), Idle);
}

#[test]
fn an_idle_input_that_went_live_during_a_widget_stall_turns_live_in_5_s_1302() {
    // a keep-alive input for 70 s (IDLE), then the widget timer stalls 70 s while the song starts at
    // 60 fps: the stall prunes the ring to one sample, IDLE holds for the first seconds after it, and
    // the fast rule turns it LIVE 5 s after the stall instead of after the full 54 s window
    let mut ring = Ring::default();
    for s in 0..70 {
        ring.tick(s * 1000, 100 + (s / 11) as u64);
    }
    assert_eq!(ring.class, Some(Idle));
    let after: Vec<_> = (140..148)
        .map(|s| ring.tick(s * 1000, 200 + 60 * (s - 100) as u64))
        .collect();
    assert_eq!(&after[..5], &[Idle; 5], "{after:?}");
    assert_eq!(&after[5..], &[Live; 3], "{after:?}");
}
