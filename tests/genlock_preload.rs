//! #70 — genlock FIFO preload reserve: the REAL zero-loss fix.
//!
//! The genlock FIFO (#42) consumed exactly one queued frame per wall-clock render
//! tick with ZERO slack, so any NDI arrival jitter emptied the queue → underrun →
//! a dropped/repeated frame (~0.38%/frame measured on each OBS hop by the #68/#69
//! QR instrument). The fix keeps a small jitter buffer: hold consumption until the
//! queue is deeper than `preload`, then consume one per tick.
//!
//! Two facets are guarded here:
//!  1. The PURE decision logic (preload parse/clamp + consume decision) is mirrored
//!     in `camera_box::probe::genlock` and unit-tested — the testable core of the
//!     fix, independent of OBS.
//!  2. A vendored-source guard (same convention as tests/obs_updater_disabled.rs)
//!     asserts the C patch is present in vendor/obs-studio so a future
//!     `git subtree pull` (#44) can't silently revert it.

use camera_box::probe::genlock::{
    parse_preload, should_consume, steady_state_depth, GENLOCK_PRELOAD_DEFAULT, GENLOCK_PRELOAD_MAX,
};

// ---- pure decision logic ---------------------------------------------------

#[test]
fn missing_or_empty_env_uses_default() {
    assert_eq!(parse_preload(None), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("   ")), GENLOCK_PRELOAD_DEFAULT);
}

#[test]
fn valid_value_is_parsed() {
    assert_eq!(parse_preload(Some("0")), 0);
    assert_eq!(parse_preload(Some("1")), 1);
    assert_eq!(parse_preload(Some("2")), 2);
    assert_eq!(parse_preload(Some("5")), 5);
}

#[test]
fn out_of_range_is_clamped_not_default() {
    // Above the cap → clamp to MAX (NOT silently fall back to default).
    assert_eq!(parse_preload(Some("99")), GENLOCK_PRELOAD_MAX);
    // Negative / garbage → default (mirrors the C strtol guard).
    assert_eq!(parse_preload(Some("-1")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("abc")), GENLOCK_PRELOAD_DEFAULT);
    assert_eq!(parse_preload(Some("3x")), GENLOCK_PRELOAD_DEFAULT);
}

#[test]
fn steady_state_at_cap_stays_below_max_async_frames() {
    // The real invariant: the steady-state queue parks at preload+1, which MUST
    // stay strictly below libobs' MAX_ASYNC_FRAMES (30). At preload == cap, depth
    // == cap+1; if that reaches MAX_ASYNC_FRAMES the FIFO force-drains every refill
    // and the source FREEZES. This catches the off-by-one (cap=29 -> depth 30 ==
    // MAX). Read the libobs literal from the vendored source so the bound tracks
    // upstream instead of being hard-coded twice.
    const LIBOBS_MAX_ASYNC_FRAMES: u32 = 30;
    assert_eq!(
        vendored_source::max_async_frames(),
        LIBOBS_MAX_ASYNC_FRAMES,
        "libobs MAX_ASYNC_FRAMES changed; re-check the GENLOCK_PRELOAD_MAX bound"
    );
    assert!(
        steady_state_depth(GENLOCK_PRELOAD_MAX) < vendored_source::max_async_frames(),
        "preload={GENLOCK_PRELOAD_MAX} steady-state depth {} reaches MAX_ASYNC_FRAMES \
         {} -> the FIFO force-drains every refill and the source FREEZES. Lower the cap.",
        steady_state_depth(GENLOCK_PRELOAD_MAX),
        vendored_source::max_async_frames()
    );
}

#[test]
fn default_is_one_frame() {
    // preload=1 → one frame of reserve = one frame of latency per hop.
    assert_eq!(GENLOCK_PRELOAD_DEFAULT, 1);
}

#[test]
fn consume_only_when_deeper_than_preload() {
    // preload = 0 reproduces the OLD zero-slack behavior: consume whenever num>0.
    assert!(!should_consume(0, 0));
    assert!(should_consume(1, 0));

    // preload = 1: hold at depth 0 and 1 (underrun), consume at depth ≥ 2.
    assert!(!should_consume(0, 1));
    assert!(!should_consume(1, 1));
    assert!(should_consume(2, 1));
    assert!(should_consume(3, 1));

    // preload = 2: hold up to and including depth 2, consume at ≥ 3.
    assert!(!should_consume(2, 2));
    assert!(should_consume(3, 2));
}

#[test]
fn steady_state_depth_is_preload_plus_one() {
    // With producer==consumer rate, the gate parks the queue one frame above the
    // reserve: depth oscillates around preload+1, leaving `preload` frames of
    // jitter slack at the moment of consumption.
    assert_eq!(steady_state_depth(0), 1);
    assert_eq!(steady_state_depth(1), 2);
    assert_eq!(steady_state_depth(2), 3);
}

// ---- vendored-source guard (the C patch must stay applied) ------------------

mod vendored_source {
    use std::path::PathBuf;

    fn vendor_file(rel: &str) -> String {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
    }

    fn squish(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
    const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";

    /// Read the libobs `#define MAX_ASYNC_FRAMES <n>` literal from the vendored
    /// source so the preload-cap invariant tracks upstream instead of being
    /// hard-coded twice.
    pub fn max_async_frames() -> u32 {
        let src = vendor_file(OBS_SOURCE);
        for line in src.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("#define MAX_ASYNC_FRAMES ") {
                return rest
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.parse::<u32>().ok())
                    .unwrap_or_else(|| {
                        panic!("could not parse MAX_ASYNC_FRAMES from {OBS_SOURCE}")
                    });
            }
        }
        panic!("MAX_ASYNC_FRAMES not found in {OBS_SOURCE}");
    }

    #[test]
    fn preload_env_is_read() {
        let src = squish(&vendor_file(OBS_SOURCE));
        assert!(
            src.contains("getenv(\"OBS_GENLOCK_PRELOAD_FRAMES\")"),
            "{OBS_SOURCE}: #70 genlock preload patch missing — OBS_GENLOCK_PRELOAD_FRAMES \
             is no longer read. A `git subtree pull` (#44) likely reverted it; re-apply."
        );
    }

    #[test]
    fn fifo_gates_on_preload_not_zero_slack() {
        let src = squish(&vendor_file(OBS_SOURCE));
        // The genlock branch must hold until the queue exceeds the preload reserve.
        assert!(
            src.contains("genlock_should_consume(source->async_frames.num, preload)"),
            "{OBS_SOURCE}: #70 — the genlock_fifo branch no longer gates consumption on \
             the preload reserve (genlock_should_consume). The zero-slack FIFO (#42) is \
             back; re-apply the #70 patch."
        );
        // The audit counters that prove underruns → 0 must still be wired.
        assert!(
            src.contains("source->genlock_underruns++"),
            "{OBS_SOURCE}: #70 — the genlock underrun audit counter is gone; re-apply."
        );
    }

    #[test]
    fn audit_counters_exist_in_struct() {
        let hdr = squish(&vendor_file(OBS_INTERNAL));
        for field in [
            "genlock_frames_received",
            "genlock_frames_consumed",
            "genlock_underruns",
            "genlock_overruns",
            "genlock_peak_depth",
        ] {
            assert!(
                hdr.contains(field),
                "{OBS_INTERNAL}: #70 audit field `{field}` missing from obs_source — \
                 an upstream subtree merge dropped the genlock audit state; re-apply."
            );
        }
    }
}
