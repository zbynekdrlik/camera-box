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
//! # Who waits (the capture loop never does)
//!
//! This module only decides the OFFSET. The wait itself lives on the send thread
//! (`crate::send_handoff`): the capture loop hands every emitted frame over with an absolute
//! deadline (emit-gate decision + offset) and returns to capture at once, and the send thread
//! waits for that deadline before the NDI send. The first version slept inside the capture loop
//! instead, so a ~15 ms per-frame work spike plus CAM7's 7.2 ms offset overran the 16.7 ms capture
//! slot (issue 1242, 24.9.2026). Every offset is still clamped to [`MAX_OFFSET_SLOT_PERCENT`]
//! (45 %) of the SHORTER of the send and capture intervals ([`slot_interval_us`]), so a send
//! starts in the first half of its own slot.
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

/// Whether the shipped build staggers at all — the one-constant rollback. ON again since the wait
/// moved to the send thread (`crate::send_handoff`, issue 1242). It was OFF for one release
/// (24.9.2026) because the first version slept INSIDE the capture loop, and a ~15 ms per-frame
/// work spike plus CAM7's 7.2 ms offset overran the capture slot.
pub const STAGGER_ACTIVE: bool = true;

/// Every offset is clamped to this percentage of the slot interval (45 % → 7.5 ms at 60 fps). A
/// frame's send therefore starts in the first half of its own slot, and the added arrival latency
/// stays bounded however many cameras share the port.
pub const MAX_OFFSET_SLOT_PERCENT: u64 = 45;
const _: () = assert!(MAX_OFFSET_SLOT_PERCENT > 0 && MAX_OFFSET_SLOT_PERCENT < 50);

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
    let send_us = match genlock_fps {
        Some(f) if f > 0 => 1_000_000 / u64::from(f),
        _ => return 0,
    };
    if capture_fps_num == 0 || capture_fps_den == 0 {
        return send_us;
    }
    let capture_us = 1_000_000 * u64::from(capture_fps_den) / u64::from(capture_fps_num);
    send_us.min(capture_us)
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
    plan_gated(
        STAGGER_ACTIVE,
        hostname,
        genlock_fps,
        capture_fps_num,
        capture_fps_den,
    )
}

/// [`plan`] with the [`STAGGER_ACTIVE`] switch as a parameter, so both positions stay tested:
/// `active` → [`plan_with`]; off → offset 0 on every box, worded
/// `NDI send stagger: cam7 offset=0 us (#1242) — stagger disabled (STAGGER_ACTIVE=false)`.
pub fn plan_gated(
    active: bool,
    hostname: &str,
    genlock_fps: Option<u32>,
    capture_fps_num: u32,
    capture_fps_den: u32,
) -> StaggerPlan {
    let planned = plan_with(hostname, genlock_fps, capture_fps_num, capture_fps_den);
    if active {
        return planned;
    }
    let name = match planned.camera {
        Some(n) => format!("cam{n}"),
        None => format!("hostname '{}'", hostname.trim()),
    };
    StaggerPlan {
        camera: planned.camera,
        offset: Duration::ZERO,
        log_line: format!(
            "NDI send stagger: {name} offset=0 us (#1242) — stagger disabled (STAGGER_ACTIVE=false)"
        ),
        warn: false,
    }
}

/// The stagger plan as the helper computes it (what `plan()` ships while [`STAGGER_ACTIVE`] is on).
pub fn plan_with(
    hostname: &str,
    genlock_fps: Option<u32>,
    capture_fps_num: u32,
    capture_fps_den: u32,
) -> StaggerPlan {
    let camera = camera_number_from_hostname(hostname);
    let slot_us = slot_interval_us(genlock_fps, capture_fps_num, capture_fps_den);
    let offset_us = send_offset_us(camera, slot_us);
    let log_line = match camera {
        Some(n) if slot_us == 0 => format!(
            "NDI send stagger: cam{n} offset={offset_us} us (#1242) — genlock off, no emit grid to stagger"
        ),
        Some(n) => format!("NDI send stagger: cam{n} offset={offset_us} us (#1242)"),
        None => format!(
            "NDI send stagger: hostname '{}' is not a CAM<N> box — offset={offset_us} us (#1242)",
            hostname.trim()
        ),
    };
    StaggerPlan {
        camera,
        offset: Duration::from_micros(offset_us),
        log_line,
        warn: camera.is_none(),
    }
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
    fn the_stagger_is_on_in_production_1242() {
        // The wait now lives on the send thread (`send_handoff`), so the capture loop never pays
        // for the offset and the shipped plan() staggers again: CAM7 = 6 x 1.2 ms.
        let p = plan("CAM7", Some(60), 60, 1);
        assert_eq!(p.offset, Duration::from_micros(7200));
        assert_eq!(p.log_line, "NDI send stagger: cam7 offset=7200 us (#1242)");
        assert!(!p.warn);
    }

    #[test]
    fn the_rollback_switch_still_words_the_disabled_line_1242() {
        // `STAGGER_ACTIVE` stays as a one-constant rollback: switched off, every box hands its
        // frame over at once and says so.
        let p = plan_gated(false, "CAM7", Some(60), 60, 1);
        assert_eq!(p.offset, Duration::ZERO);
        assert_eq!(
            p.log_line,
            "NDI send stagger: cam7 offset=0 us (#1242) — stagger disabled (STAGGER_ACTIVE=false)"
        );
        assert!(!p.warn);
        assert_eq!(
            plan_gated(true, "CAM7", Some(60), 60, 1),
            plan_with("CAM7", Some(60), 60, 1)
        );
    }

    #[test]
    fn plan_resolves_offset_and_the_one_startup_line_1242() {
        let p = plan_with("CAM7", Some(60), 60, 1);
        assert_eq!(p.camera, Some(7));
        assert_eq!(p.offset, Duration::from_micros(7200));
        assert_eq!(p.log_line, "NDI send stagger: cam7 offset=7200 us (#1242)");
        assert!(!p.warn);

        let p = plan_with("CAM1", Some(60), 60, 1);
        assert_eq!(p.offset, Duration::ZERO);
        assert_eq!(p.log_line, "NDI send stagger: cam1 offset=0 us (#1242)");
        assert!(!p.warn);
    }

    #[test]
    fn plan_names_genlock_off_and_an_unknown_hostname_1242() {
        let p = plan_with("CAM7", None, 60, 1);
        assert_eq!(p.offset, Duration::ZERO);
        assert!(
            p.log_line.contains("offset=0 us (#1242)") && p.log_line.contains("genlock off"),
            "{}",
            p.log_line
        );
        assert!(!p.warn);

        let p = plan_with(" camera-box ", Some(60), 60, 1);
        assert_eq!(p.camera, None);
        assert_eq!(p.offset, Duration::ZERO);
        assert!(p.log_line.contains("'camera-box'"), "{}", p.log_line);
        assert!(p.log_line.contains("offset=0 us (#1242)"), "{}", p.log_line);
        assert!(p.warn);
    }
}
