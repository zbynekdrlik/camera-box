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
//! still grids its next boundary from the wall clock, never from "last send + interval". Receivers
//! hold by timecode, not by arrival, so only the arrival instant moves (≤ 7.2 ms, well inside the
//! ~65 ms arrival floor).
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
//! The upper bound on the offset is the capture loop's own honesty: the send is synchronous on the
//! capture thread, so the delay shortens the NEXT V4L2 dequeue wait. The #1131 "was the frame
//! already buffered" signal (`capture_stall::frame_from_nonempty_queue`, fraction 0.5 of an
//! interval) is kept honest by adding the delay back ([`idle_wait_ms`]). That correction is only
//! sound while the delay itself stays BELOW half an interval, so every offset is clamped to
//! [`MAX_OFFSET_SLOT_PERCENT`] (45 %) of the send interval (7.5 ms at 60 fps).
//!
//! # Identity
//!
//! The camera number comes from the box's own OS hostname (`CAM1`..`CAM7`, set by
//! `scripts/setup-device.sh`, the same `gethostname(2)` value the NDI SDK publishes as the source's
//! machine name). One rule for every box — no per-box table, no env knob. An unknown or unparsable
//! hostname gets offset 0 (never a panic), and the caller logs that once at startup.

use std::time::Duration;

/// Spacing between consecutive cameras' send instants, in microseconds. See the module doc for
/// the 1 GbE wire-time vs 2.5 GbE drain arithmetic.
pub const STAGGER_US: u64 = 1200;

/// Every offset is clamped to this percentage of the send interval (45 % → 7.5 ms at 60 fps), so the
/// frame is always handed over well inside its own slot AND the [`idle_wait_ms`] correction of the
/// #1131 buffered-queue signal stays sound (it needs the delay below half an interval).
pub const MAX_OFFSET_SLOT_PERCENT: u64 = 45;
// The clamp must stay strictly below `capture_stall::BUFFERED_DEQUEUE_FRACTION` (0.5), or the
// `idle_wait_ms` correction could read a genuinely buffered frame as an empty-queue wait.
const _: () = assert!(MAX_OFFSET_SLOT_PERCENT > 0 && MAX_OFFSET_SLOT_PERCENT < 50);

/// The highest camera number the parser accepts. Keeps a garbage hostname like `CAM4294967295`
/// from being read as a real camera; any real cambox is far below this.
pub const MAX_CAMERA_NUMBER: u32 = 99;

/// Parse the camera number from a box hostname: `CAM7` / `cam7` / ` Cam3 ` → `Some(n)`.
/// Anything else (`camera-box`, `CAM`, `CAM0`, `CAM1-lx`, `strih-lx`, empty) → `None`.
pub fn camera_number_from_hostname(hostname: &str) -> Option<u32> {
    // #1242 RED stub: the parser is not written yet.
    let _ = hostname;
    None
}

/// The send offset (µs) for camera `camera_number` at a send interval of `interval_us`.
///
/// `(n − 1) × STAGGER_US`, clamped to [`MAX_OFFSET_SLOT_PERCENT`] of the interval. `None` (unknown
/// identity) or `interval_us == 0` (genlock off — no emit grid to stagger inside) → 0. Monotonic
/// non-decreasing in `n`.
pub fn send_offset_us(camera_number: Option<u32>, interval_us: u64) -> u64 {
    // #1242 RED stub: no stagger yet.
    let _ = (camera_number, interval_us);
    0
}

/// How long to still sleep before the NDI hand-off: `offset` measured from the emit-gate anchor,
/// minus what already `elapsed` since that anchor (saturating at zero). Anchoring to the gate
/// instant keeps the delay precise regardless of the small per-frame work between the gate and
/// the send.
pub fn remaining_sleep(offset: Duration, elapsed: Duration) -> Duration {
    // #1242 RED stub.
    let _ = (offset, elapsed);
    Duration::ZERO
}

/// The capture loop's idle wait before this frame, for the #1131 buffered-queue signal: the
/// measured V4L2 dequeue wait plus the stagger sleep the loop spent just before it. Without the
/// stagger the loop would have reached the dequeue that much earlier and waited that much longer,
/// so a healthy empty-queue frame must not read as "already buffered" merely because the loop
/// slept first. Non-finite or negative inputs count as 0 for the stagger term, and the dequeue term
/// passes through unchanged so `frame_from_nonempty_queue`'s own fail-safe guards still see it.
pub fn idle_wait_ms(dequeue_ms: f64, stagger_slept_ms: f64) -> f64 {
    // #1242 RED stub: the stagger sleep is not added back yet.
    let _ = stagger_slept_ms;
    dequeue_ms
}

/// The ONE startup log line (logged once). `Some(n)` → `NDI send stagger: camN offset=<us> us
/// (#1242)`; `None` → names the hostname and says the offset is 0.
pub fn startup_log_line(hostname: &str, camera_number: Option<u32>, offset_us: u64) -> String {
    // #1242 RED stub.
    let _ = (hostname, camera_number, offset_us);
    String::new()
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
    fn a_30fps_slot_keeps_the_same_spacing_1242() {
        let slot_30 = 1_000_000 / 30;
        assert_eq!(send_offset_us(Some(7), slot_30), 7200);
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
    fn startup_line_names_the_camera_and_offset_1242() {
        assert_eq!(
            startup_log_line("CAM7", Some(7), 7200),
            "NDI send stagger: cam7 offset=7200 us (#1242)"
        );
        assert_eq!(
            startup_log_line("CAM1", Some(1), 0),
            "NDI send stagger: cam1 offset=0 us (#1242)"
        );
        let unknown = startup_log_line(" camera-box ", None, 0);
        assert!(unknown.contains("'camera-box'"), "{unknown}");
        assert!(unknown.contains("offset=0 us (#1242)"), "{unknown}");
    }
}
