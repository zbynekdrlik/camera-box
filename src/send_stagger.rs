//! #1242 — per-camera NDI SEND stagger inside the genlock frame slot (pure decision).
//!
//! # Why
//!
//! Every cambox emits on the SAME 60 Hz genlock grid, and on the development rig every cambox
//! captures the SAME camera through an HDMI splitter, so all of them hand their frame to the NDI
//! SDK within ~1 ms of each other. Each frame is ~300–350 KB of SpeedHQ on the wire. The seven
//! bursts cross the 10 G trunks together and converge on ONE 2.5 GbE egress port: the strih-lx
//! uplink `foh1_video ether2`. That burst overflows the switch egress queue and the switch
//! tail-drops whole packet runs (`tx-drop-queue1` climbing, up to ~2500 packets per burst). A large
//! run costs the affected receivers ~380 ms of video on 2–5 cameras at once. See the
//! `.claude/rules/ndi-send-stagger.md` rule for the evidence chain.
//!
//! # What this changes (and what it does NOT)
//!
//! Camera N hands its frame to the NDI SDK `(N−1) × STAGGER_US` after its emit-gate decision. The
//! emit grid and the FLOOR-boundary NDI timecode are UNCHANGED (docs/genlock-sender-contract.md):
//! the timecode is computed from the capture instant BEFORE the delay, and the decimation gate
//! still grids its next boundary from the wall clock, never from "last send + interval". What DOES
//! move is the arrival at the receivers: a later camera's frames reach strih up to 7.2 ms later.
//! The per-run `[4i/8align]` step (relative, floor-3 pins) re-equalises the presented alignment;
//! until it has, that offset counts against the blocking cross-camera delivery-spread budget
//! (`switch_latency::SPREAD_THRESHOLD_MS`, 24 ms).
//!
//! # The arithmetic behind `STAGGER_US = 1200`
//!
//! A cambox NIC links at 1 GbE, so one ~300–350 KB frame takes 2.4–2.8 ms on its wire. The 2.5 G
//! egress drains one such frame in ~1.0–1.1 ms. With the senders spaced `s` apart, the aggregate
//! arrival rate during the burst train is about `wire_time / s × 1 Gb/s`. It stays at or below the
//! 2.5 Gb/s drain once `s ≥ wire_time / 2.5` = 0.96–1.12 ms. At `s` = 1.2 ms the train peaks at
//! 2.0–2.33 Gb/s, so the egress queue no longer builds. Seven cameras then span 6 × 1.2 = 7.2 ms of
//! send offset (≈ 9.6–10 ms of wire time including the last frame), inside the 16.67 ms slot.
//!
//! # The capture-loop budget (why the clamp and the backlog skip exist)
//!
//! The send is synchronous on the capture thread, which is also the only thread that drains the
//! V4L2 queue. The sleep is therefore dead time for the capture loop:
//!
//! 1. It shortens the NEXT dequeue wait. The #1131 "was the frame already buffered" signal
//!    (`capture_stall::frame_from_nonempty_queue`, 0.5 of a capture interval) is kept honest by
//!    adding the sleep back ([`idle_wait_ms`]). That is only sound while the sleep stays BELOW half
//!    a capture interval, so every offset is clamped to [`MAX_OFFSET_SLOT_PERCENT`] (45 %) of the
//!    SHORTER of the send and capture intervals ([`slot_interval_us`]). `lib.rs` pins the clamp
//!    against the real `BUFFERED_DEQUEUE_FRACTION` at compile time.
//! 2. A frame that ALREADY came from a non-empty queue means the loop is behind; sleeping would
//!    push it further behind and could cost captured frames. [`should_sleep`] skips the stagger
//!    for that frame, and the skip is counted in the 5 s [`window_summary`].
//! 3. [`StaggerWindow`] records the per-frame work (callback time minus the sleep) so the margin
//!    left on the latest camera is visible on every box, and [`window_summary`] WARNs when the
//!    worst frame's work plus the offset reaches a capture interval.
//!
//! # Identity
//!
//! The camera number comes from the box's own OS hostname (`CAM1`..`CAM7`, set by
//! `scripts/setup-device.sh`, the same `gethostname(2)` value the NDI SDK publishes as the source's
//! machine name). One rule for every box — no per-box table, no env knob. An unknown or unparsable
//! hostname gets offset 0 (never a panic), and [`plan`] words the one startup log line.

use std::time::Duration;

/// Spacing between consecutive cameras' send instants, in microseconds. See the module doc for
/// the 1 GbE wire-time vs 2.5 GbE drain arithmetic.
pub const STAGGER_US: u64 = 1200;

/// Every offset is clamped to this percentage of the slot interval (45 % → 7.5 ms at 60 fps), so the
/// frame is always handed over well inside its own slot AND the [`idle_wait_ms`] correction of the
/// #1131 buffered-queue signal stays sound (it needs the sleep below half a capture interval; the
/// cross-module compile-time check against `capture_stall::BUFFERED_DEQUEUE_FRACTION` lives in
/// `lib.rs`).
pub const MAX_OFFSET_SLOT_PERCENT: u64 = 45;
const _: () = assert!(MAX_OFFSET_SLOT_PERCENT > 0 && MAX_OFFSET_SLOT_PERCENT < 100);

/// The highest camera number the parser accepts. Keeps a garbage hostname like `CAM4294967295`
/// from being read as a real camera; any real cambox is far below this.
pub const MAX_CAMERA_NUMBER: u32 = 99;

/// Parse the camera number from a box hostname: `CAM7` / `cam7` / ` Cam3 ` → `Some(n)`.
/// Anything else (`camera-box`, `CAM`, `CAM0`, `CAM1-lx`, `strih-lx`, empty) → `None`.
pub fn camera_number_from_hostname(hostname: &str) -> Option<u32> {
    let h = hostname.trim();
    if h.len() < 4 || !h.is_char_boundary(3) || !h[..3].eq_ignore_ascii_case("cam") {
        return None;
    }
    let digits = &h[3..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if !(1..=MAX_CAMERA_NUMBER).contains(&n) {
        return None;
    }
    Some(n)
}

/// The interval (µs) the offset is clamped inside: the SHORTER of the genlock send interval and
/// the negotiated capture interval (`capture_fps_num / capture_fps_den` fps). Genlock off
/// (`None` or 0 fps) → 0, i.e. no emit grid to stagger inside. An unknown capture rate (a zero
/// numerator or denominator) falls back to the send interval alone.
pub fn slot_interval_us(
    genlock_fps: Option<u32>,
    capture_fps_num: u32,
    capture_fps_den: u32,
) -> u64 {
    // #1242 review RED stub.
    let _ = (genlock_fps, capture_fps_num, capture_fps_den);
    0
}

/// The send offset (µs) for camera `camera_number` inside a slot of `slot_us` ([`slot_interval_us`]).
///
/// `(n − 1) × STAGGER_US`, clamped to [`MAX_OFFSET_SLOT_PERCENT`] of the slot. `None` (unknown
/// identity) or `slot_us == 0` (genlock off) → 0. Monotonic non-decreasing in `n`.
pub fn send_offset_us(camera_number: Option<u32>, slot_us: u64) -> u64 {
    let n = match camera_number {
        Some(n) if n >= 1 => n,
        _ => return 0,
    };
    if slot_us == 0 {
        return 0;
    }
    let raw = u64::from(n - 1).saturating_mul(STAGGER_US);
    let cap = slot_us.saturating_mul(MAX_OFFSET_SLOT_PERCENT) / 100;
    raw.min(cap)
}

/// The resolved per-box stagger: the offset plus the ONE startup log line and whether it is a
/// WARN (no CAM<N> identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaggerPlan {
    pub camera: Option<u32>,
    pub offset: Duration,
    pub log_line: String,
    pub warn: bool,
}

/// Resolve the stagger for this box from its hostname, the genlock send rate and the negotiated
/// capture rate. Log line shapes:
/// `NDI send stagger: cam7 offset=7200 us (#1242)` (genlock on);
/// `... offset=0 us (#1242) — genlock off, no emit grid to stagger` (known camera, genlock off);
/// `NDI send stagger: hostname 'x' is not a CAM<N> box — offset=0 us (#1242)` (WARN).
pub fn plan(
    hostname: &str,
    genlock_fps: Option<u32>,
    capture_fps_num: u32,
    capture_fps_den: u32,
) -> StaggerPlan {
    // #1242 review RED stub.
    let _ = (hostname, genlock_fps, capture_fps_num, capture_fps_den);
    StaggerPlan {
        camera: None,
        offset: Duration::ZERO,
        log_line: String::new(),
        warn: false,
    }
}

/// The pre-review startup line builder, still called by the capture-loop wiring until the
/// GREEN step switches it to [`plan`].
pub fn startup_log_line(hostname: &str, camera_number: Option<u32>, offset_us: u64) -> String {
    match camera_number {
        Some(n) => format!("NDI send stagger: cam{n} offset={offset_us} us (#1242)"),
        None => format!(
            "NDI send stagger: hostname '{}' is not a CAM<N> box — offset={offset_us} us (#1242)",
            hostname.trim()
        ),
    }
}

/// Should this emitted iteration sleep for its stagger? Only with a non-zero offset AND a frame
/// that did NOT already come from a non-empty V4L2 queue: a backlogged frame means the capture
/// loop is behind, and sleeping would push it further behind (the capture budget beats the
/// network optimisation). The caller counts the skip.
pub fn should_sleep(offset: Duration, frame_backlogged: bool) -> bool {
    // #1242 review RED stub: the backlog is ignored (the pre-review behaviour).
    let _ = frame_backlogged;
    !offset.is_zero()
}

/// How long to still sleep before the NDI hand-off: `offset` measured from the emit-gate anchor,
/// minus what already `elapsed` since that anchor (saturating at zero). Anchoring to the gate
/// instant keeps the delay precise regardless of the small per-frame work between the gate and
/// the send.
pub fn remaining_sleep(offset: Duration, elapsed: Duration) -> Duration {
    offset.saturating_sub(elapsed)
}

/// The capture loop's idle wait before this frame, for the #1131 buffered-queue signal: the
/// measured V4L2 dequeue wait plus the stagger sleep the loop spent just before that dequeue.
/// Without the stagger the loop would have reached the dequeue that much earlier and waited that
/// much longer, so a healthy empty-queue frame must not read as "already buffered" merely because
/// the loop slept first. Non-finite or negative inputs count as 0 for the stagger term, and the
/// dequeue term passes through unchanged so `frame_from_nonempty_queue`'s own fail-safe guards
/// still see it.
pub fn idle_wait_ms(dequeue_ms: f64, stagger_slept_ms: f64) -> f64 {
    if !stagger_slept_ms.is_finite() || stagger_slept_ms <= 0.0 {
        return dequeue_ms;
    }
    dequeue_ms + stagger_slept_ms
}

/// Per-5 s-window stagger accounting, drained by the capture loop's routine report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaggerWindow {
    /// Emitted iterations that slept for their stagger.
    pub slept: u64,
    /// Emitted iterations that skipped the stagger because the frame was already backlogged.
    pub skipped_backlogged: u64,
    /// The worst emitted iteration's work (callback time minus the stagger sleep), in ms.
    pub max_work_ms: f64,
}

impl StaggerWindow {
    pub fn note_slept(&mut self) {
        // #1242 review RED stub.
    }

    pub fn note_skipped(&mut self) {
        // #1242 review RED stub.
    }

    /// Record one emitted iteration's work. A non-finite or negative reading is ignored.
    pub fn note_work(&mut self, work_ms: f64) {
        // #1242 review RED stub.
        let _ = work_ms;
    }

    /// Drain the window (returns it and resets to empty).
    pub fn take(&mut self) -> StaggerWindow {
        std::mem::take(self)
    }
}

/// The 5 s summary line for a drained window, and whether it is a WARN. It WARNs when any frame
/// skipped its stagger (the loop was behind) or when the worst frame's work plus the offset
/// reached a full capture interval (no margin left). `capture_interval_ms <= 0` never WARNs on the
/// budget term.
pub fn window_summary(
    w: &StaggerWindow,
    offset_us: u64,
    capture_interval_ms: f64,
) -> (String, bool) {
    // #1242 review RED stub.
    let _ = (w, offset_us, capture_interval_ms);
    (String::new(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT_60_US: u64 = 1_000_000 / 60; // 16_666 µs

    #[test]
    fn parses_the_fleet_hostname_convention_1242() {
        assert_eq!(camera_number_from_hostname("CAM1"), Some(1));
        assert_eq!(camera_number_from_hostname("CAM7"), Some(7));
        assert_eq!(camera_number_from_hostname("cam3"), Some(3));
        assert_eq!(camera_number_from_hostname(" Cam5 \n"), Some(5));
        assert_eq!(camera_number_from_hostname("CAM07"), Some(7));
        assert_eq!(camera_number_from_hostname("CAM12"), Some(12));
    }

    #[test]
    fn unknown_or_garbage_hostname_is_none_never_a_panic_1242() {
        for h in [
            "",
            "   ",
            "CAM",
            "CAM0",
            "camera-box",
            "CAM1-lx",
            "strih-lx",
            "cam 1",
            "CAM-1",
            "CAM4294967295",
            "CAM100",
            "čam1",
            "ca",
            "C",
        ] {
            assert_eq!(camera_number_from_hostname(h), None, "hostname {h:?}");
        }
    }

    #[test]
    fn cam1_is_the_unshifted_anchor_1242() {
        assert_eq!(send_offset_us(Some(1), SLOT_60_US), 0);
    }

    #[test]
    fn offset_is_n_minus_one_stagger_steps_1242() {
        assert_eq!(send_offset_us(Some(2), SLOT_60_US), STAGGER_US);
        assert_eq!(send_offset_us(Some(4), SLOT_60_US), 3 * STAGGER_US);
        assert_eq!(send_offset_us(Some(7), SLOT_60_US), 6 * STAGGER_US);
        assert_eq!(send_offset_us(Some(7), SLOT_60_US), 7200);
    }

    #[test]
    fn offset_is_monotonic_in_camera_number_1242() {
        let mut prev = 0;
        for n in 1..=MAX_CAMERA_NUMBER {
            let o = send_offset_us(Some(n), SLOT_60_US);
            assert!(o >= prev, "cam{n} offset {o} < cam{} offset {prev}", n - 1);
            prev = o;
        }
    }

    #[test]
    fn the_seven_camera_fleet_gets_seven_distinct_slots_1242() {
        let offsets: Vec<u64> = (1..=7)
            .map(|n| send_offset_us(Some(n), SLOT_60_US))
            .collect();
        for w in offsets.windows(2) {
            assert!(
                w[1] > w[0],
                "cameras 1..7 must be strictly spaced: {offsets:?}"
            );
            assert!(
                w[1] - w[0] >= 1000,
                "spacing must exceed the ~1 ms 2.5 GbE drain time of one frame: {offsets:?}"
            );
        }
    }

    #[test]
    fn offset_is_clamped_well_inside_the_slot_1242() {
        let cap = SLOT_60_US * MAX_OFFSET_SLOT_PERCENT / 100;
        for n in 1..=MAX_CAMERA_NUMBER {
            let o = send_offset_us(Some(n), SLOT_60_US);
            assert!(o <= cap, "cam{n} offset {o} exceeds the clamp {cap}");
            // Strictly below half an interval: the idle_wait_ms correction relies on it.
            assert!(
                2 * o < SLOT_60_US,
                "cam{n} offset {o} not below half the slot"
            );
        }
        assert_eq!(send_offset_us(Some(50), SLOT_60_US), cap);
    }

    #[test]
    fn unknown_identity_or_genlock_off_is_zero_1242() {
        assert_eq!(send_offset_us(None, SLOT_60_US), 0);
        assert_eq!(send_offset_us(Some(0), SLOT_60_US), 0);
        assert_eq!(send_offset_us(Some(7), 0), 0);
        assert_eq!(send_offset_us(None, 0), 0);
    }

    #[test]
    fn slot_is_the_shorter_of_send_and_capture_interval_1242() {
        // 60 fps genlock on a 60 fps capture.
        assert_eq!(slot_interval_us(Some(60), 60, 1), 16_666);
        // 30 fps genlock on a 60 fps capture: the CAPTURE interval bounds the sleep.
        assert_eq!(slot_interval_us(Some(30), 60, 1), 16_666);
        // A 60000/1001 capture is slightly longer than the 60 fps send interval.
        assert_eq!(slot_interval_us(Some(60), 60_000, 1001), 16_666);
        // Genlock off or 0 fps → no grid.
        assert_eq!(slot_interval_us(None, 60, 1), 0);
        assert_eq!(slot_interval_us(Some(0), 60, 1), 0);
        // Unknown capture rate → the send interval alone.
        assert_eq!(slot_interval_us(Some(60), 0, 1), 16_666);
        assert_eq!(slot_interval_us(Some(60), 60, 0), 16_666);
    }

    #[test]
    fn a_30fps_genlock_is_clamped_by_the_60fps_capture_interval_1242() {
        let slot = slot_interval_us(Some(30), 60, 1);
        assert_eq!(send_offset_us(Some(7), slot), 7200);
        // A large camera number never exceeds 45 % of the CAPTURE interval (7.5 ms), not the
        // 15 ms that 45 % of the 33.3 ms send interval would allow.
        assert_eq!(send_offset_us(Some(50), slot), 7499);
    }

    #[test]
    fn plan_resolves_offset_and_the_one_startup_line_1242() {
        let p = plan("CAM7", Some(60), 60, 1);
        assert_eq!(p.camera, Some(7));
        assert_eq!(p.offset, Duration::from_micros(7200));
        assert_eq!(p.log_line, "NDI send stagger: cam7 offset=7200 us (#1242)");
        assert!(!p.warn);

        let p = plan("CAM1", Some(60), 60, 1);
        assert_eq!(p.offset, Duration::ZERO);
        assert_eq!(p.log_line, "NDI send stagger: cam1 offset=0 us (#1242)");
        assert!(!p.warn);
    }

    #[test]
    fn plan_names_genlock_off_and_an_unknown_hostname_1242() {
        let p = plan("CAM7", None, 60, 1);
        assert_eq!(p.offset, Duration::ZERO);
        assert!(
            p.log_line.contains("offset=0 us (#1242)") && p.log_line.contains("genlock off"),
            "{}",
            p.log_line
        );
        assert!(!p.warn);

        let p = plan(" camera-box ", Some(60), 60, 1);
        assert_eq!(p.camera, None);
        assert_eq!(p.offset, Duration::ZERO);
        assert!(p.log_line.contains("'camera-box'"), "{}", p.log_line);
        assert!(p.log_line.contains("offset=0 us (#1242)"), "{}", p.log_line);
        assert!(p.warn);
    }

    #[test]
    fn a_backlogged_frame_skips_the_stagger_1242() {
        let off = Duration::from_micros(7200);
        assert!(should_sleep(off, false));
        assert!(!should_sleep(off, true));
        assert!(!should_sleep(Duration::ZERO, false));
        assert!(!should_sleep(Duration::ZERO, true));
    }

    #[test]
    fn remaining_sleep_counts_from_the_gate_anchor_1242() {
        let off = Duration::from_micros(7200);
        assert_eq!(
            remaining_sleep(off, Duration::from_micros(200)),
            Duration::from_micros(7000)
        );
        assert_eq!(remaining_sleep(off, off), Duration::ZERO);
        assert_eq!(
            remaining_sleep(off, Duration::from_millis(20)),
            Duration::ZERO
        );
        assert_eq!(
            remaining_sleep(Duration::ZERO, Duration::from_micros(5)),
            Duration::ZERO
        );
    }

    #[test]
    fn idle_wait_adds_the_stagger_sleep_back_1242() {
        // A healthy empty-queue frame on cam7: the loop slept 7.2 ms, then waited 5 ms in the
        // dequeue. Without the stagger it would have waited ~12.2 ms → NOT buffered (≥ 8.33 ms).
        assert!((idle_wait_ms(5.0, 7.2) - 12.2).abs() < 1e-9);
        // A genuinely buffered frame: dequeue 0, slept 7.2 → 7.2 ms, still below half a slot.
        assert!(idle_wait_ms(0.0, 7.2) < 16.667 * 0.5);
        // No stagger (cam1, a decimated previous iteration, or genlock off) → unchanged.
        assert_eq!(idle_wait_ms(9.5, 0.0), 9.5);
    }

    #[test]
    fn idle_wait_ignores_a_bad_stagger_measurement_1242() {
        assert_eq!(idle_wait_ms(4.0, -1.0), 4.0);
        assert_eq!(idle_wait_ms(4.0, f64::NAN), 4.0);
        assert_eq!(idle_wait_ms(4.0, f64::INFINITY), 4.0);
        // The dequeue term passes through untouched, so the downstream guard still sees a NaN.
        assert!(idle_wait_ms(f64::NAN, 1.0).is_nan());
    }

    #[test]
    fn window_counts_and_drains_1242() {
        let mut w = StaggerWindow::default();
        w.note_slept();
        w.note_slept();
        w.note_skipped();
        w.note_work(3.0);
        w.note_work(5.5);
        w.note_work(4.0);
        w.note_work(f64::NAN);
        w.note_work(-1.0);
        let d = w.take();
        assert_eq!(d.slept, 2);
        assert_eq!(d.skipped_backlogged, 1);
        assert_eq!(d.max_work_ms, 5.5);
        assert_eq!(w, StaggerWindow::default());
    }

    #[test]
    fn window_summary_is_info_with_margin_left_1242() {
        let w = StaggerWindow {
            slept: 300,
            skipped_backlogged: 0,
            max_work_ms: 3.1,
        };
        let (line, warn) = window_summary(&w, 7200, 16.667);
        assert!(!warn, "{line}");
        assert!(line.starts_with("#1242 send stagger: offset=7200 us, 300 slept / 0 skipped"));
        assert!(
            line.contains("max per-frame work 3.1 ms + offset 7.2 ms vs capture interval 16.7 ms")
        );
        assert!(!line.contains("OVER BUDGET"), "{line}");
    }

    #[test]
    fn window_summary_warns_on_skips_or_no_margin_1242() {
        let skipped = StaggerWindow {
            slept: 290,
            skipped_backlogged: 10,
            max_work_ms: 3.0,
        };
        let (_, warn) = window_summary(&skipped, 7200, 16.667);
        assert!(warn);

        let tight = StaggerWindow {
            slept: 300,
            skipped_backlogged: 0,
            max_work_ms: 9.5,
        };
        let (line, warn) = window_summary(&tight, 7200, 16.667);
        assert!(warn);
        assert!(line.contains("OVER BUDGET"), "{line}");

        // No capture interval known → never a budget WARN.
        let (_, warn) = window_summary(&tight, 7200, 0.0);
        assert!(!warn);
    }
}
