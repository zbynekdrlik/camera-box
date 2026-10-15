//! Issue 1404 Task 5 part b — `recording-verdict --av-sync --av-run <run>`: the A/V of a
//! SELF-MARKED run, paired through that run's own dual-QR tick (design comment 6048239795).
//!
//! The camera-box measurement clip (`scripts/gen_measurement_clip.py`, run 911016) is a synthesized
//! 30 fps recording of the cam2 painter: its dual-QR Vernier carries the 60 Hz tick and its sound
//! carries the QPSK marker, index = tick & 0xFF, every 30 ticks. SongPlayer / the cg OBS play it in
//! the CG segments, so one stream recording of a CG segment holds BOTH the clip's picture tick and
//! its own marker: its A/V is measured from that one file, with the clip's marker log as the emit
//! log. The painter path reads `RecordingFrame::tick`, which excludes every reserved id
//! (`NODE_BURN_RUN_IDS`, 911016 included), so the clip needs this explicit selector. The painter
//! path stays the default and is unchanged.
//!
//! Pure (no I/O): the probe glue (`probe::av_sync_recording::av_sync_from_recording`) decodes the
//! frames and the audio, then pairs through the same crate-root calls as the painter path
//! (`qpsk_marker::av_offset_candidates_deduped` + `cluster_offset_ms`). Only the tick source
//! differs, and it lives here:
//! - [`run_frame_tick`]: a frame's tick of the selected run (the freshest Vernier half);
//! - [`run_tick_samples`]: the `(tick, video_ts)` samples, refused when the run's tick shows again
//!   later (the clip restarts its tick on every play, so a looped or replayed clip names two video
//!   times per tick and would pair every marker twice).

use crate::av_window::window_ticks;

/// The runs that carry their OWN QPSK marker next to their dual-QR tick, so one recording pairs
/// picture against sound: only the measurement clip. Mirrors
/// `probe::recording_latency::MEASUREMENT_CLIP_RUN_ID` (pinned by `tests/av_run_pairing_clip_1404.rs`)
/// and `youtube_leg_ticks.CLIP_RUNS` (pinned by `tests/python/test_reserved_origin_runs_1404.py`).
pub const SELF_MARKED_RUN_IDS: [u32; 1] = [911_016];

/// A tick of the selected run seen again more than this long after its first frame is a restart of
/// the run (the clip played again or looped), never a repeated frame: a held or repeated frame shows
/// its tick again within a few frames.
pub const RESTART_GAP_S: f64 = 2.0;

/// Ok when `run` may be passed to `--av-run` (a [`SELF_MARKED_RUN_IDS`] member), else the reason.
pub fn check_av_run(run: u32) -> Result<(), String> {
    if SELF_MARKED_RUN_IDS.contains(&run) {
        Ok(())
    } else {
        Err(format!(
            "--av-run {run}: not a self-marked run (one whose recording carries its own QPSK marker \
             next to its dual-QR tick); the only one is the measurement clip, {:?}",
            SELF_MARKED_RUN_IDS
        ))
    }
}

/// The tick of one decoded frame for `run`: the highest `frame_id` among that run's payloads (the
/// freshest Vernier half, as `RecordingFrame::tick` takes it for the painter). `None` when the frame
/// shows no payload of `run`. `payloads` are `(run_id, frame_id)` pairs.
pub fn run_frame_tick<I>(payloads: I, run: u32) -> Option<u32>
where
    I: IntoIterator<Item = (u32, u32)>,
{
    payloads
        .into_iter()
        .filter(|&(r, _)| r == run)
        .map(|(_, frame_id)| frame_id)
        .max()
}

/// `(tick, video_ts)` samples of one run's frames (`(frame_index, tick)` in file order): the first
/// frame per tick, sorted by tick (`av_window::window_ticks`, the same construction the painter path
/// in `probe::av_sync_recording` writes inline), or an error when a tick shows again more than
/// [`RESTART_GAP_S`] after its first frame: the run restarted inside the recording, and a marker
/// could pair with either play.
pub fn run_tick_samples(
    frames: &[(u64, Option<u32>)],
    fps: f64,
    video_start_s: f64,
) -> Result<Vec<(u32, f64)>, String> {
    if !(fps.is_finite() && fps > 0.0) {
        return Err(format!("video fps {fps} is not a positive number"));
    }
    let mut first: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for &(frame_index, tick) in frames {
        let Some(t) = tick else { continue };
        let at = *first.entry(t).or_insert(frame_index);
        let gap_s = frame_index.abs_diff(at) as f64 / fps;
        if gap_s > RESTART_GAP_S {
            return Err(format!(
                "the run's tick {t} shows again {gap_s:.1} s after its first frame (frames {at} and \
                 {frame_index}): the run restarted inside this recording (a looped or replayed clip); \
                 measure a cut that holds one play"
            ));
        }
    }
    Ok(window_ticks(frames, fps, video_start_s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: u32 = 911_016;
    const PAINTER: u32 = 123_456_789;

    /// The clip's dual-QR of frame f: left = tick 2f (even), right = 2f - 1 (`vernier_ids(2f)`).
    fn clip_frame(f: u32) -> Vec<(u32, u32)> {
        if f == 0 {
            vec![(CLIP, 0)]
        } else {
            vec![(CLIP, 2 * f), (CLIP, 2 * f - 1)]
        }
    }

    #[test]
    fn only_the_measurement_clip_may_be_selected() {
        assert_eq!(check_av_run(911_016), Ok(()));
        for run in [911_014, 911_015, 911_001, 911_013, PAINTER, 0] {
            let err = check_av_run(run).expect_err("refused");
            assert!(
                err.contains(&run.to_string()) && err.contains("911016"),
                "{err}"
            );
        }
    }

    #[test]
    fn a_frame_tick_is_the_freshest_half_of_the_selected_run_only() {
        assert_eq!(run_frame_tick(clip_frame(7), CLIP), Some(14));
        // a painter QR and a burn on the same frame never become the clip's tick, and vice versa
        let mixed = vec![(PAINTER, 900_000), (911_002, 50), (CLIP, 14), (CLIP, 13)];
        assert_eq!(run_frame_tick(mixed.clone(), CLIP), Some(14));
        assert_eq!(run_frame_tick(mixed, PAINTER), Some(900_000));
        assert_eq!(run_frame_tick(vec![(PAINTER, 900_000)], CLIP), None);
        assert_eq!(run_frame_tick(Vec::new(), CLIP), None);
    }

    #[test]
    fn samples_are_the_first_frame_per_tick_on_the_container_timeline() {
        // 30 fps, the clip's 60 Hz tick: frame f shows tick 2f; frame 3 repeated (a rig repeat)
        let mut frames: Vec<(u64, Option<u32>)> = (0..6u32)
            .map(|f| (u64::from(f), run_frame_tick(clip_frame(f), CLIP)))
            .collect();
        frames.insert(4, (4, Some(6)));
        frames.push((7, None));
        let s = run_tick_samples(&frames, 30.0, 0.5).expect("one play");
        let ticks: Vec<u32> = s.iter().map(|&(t, _)| t).collect();
        assert_eq!(ticks, vec![0, 2, 4, 6, 8, 10]);
        let t6 = s.iter().find(|&&(t, _)| t == 6).expect("tick 6").1;
        assert!(
            (t6 - (0.5 + 3.0 / 30.0)).abs() < 1e-12,
            "tick 6 at its FIRST frame: {t6}"
        );
    }

    #[test]
    fn a_restarted_run_is_refused_never_paired_twice() {
        // the clip plays 0..=99 frames, then loops back to its start
        let mut frames: Vec<(u64, Option<u32>)> =
            (0..100u32).map(|f| (u64::from(f), Some(2 * f))).collect();
        frames.extend((0..30u32).map(|f| (u64::from(100 + f), Some(2 * f))));
        let err = run_tick_samples(&frames, 30.0, 0.0).expect_err("a loop");
        assert!(
            err.contains("restarted") && err.contains("tick 0") && err.contains("frames 0 and 100"),
            "{err}"
        );
        // a tick shown again just under the bound (a held picture) is a repeat, not a restart
        let held: Vec<(u64, Option<u32>)> =
            vec![(0, Some(10)), (1, Some(12)), (60, Some(12)), (61, Some(14))];
        assert!(run_tick_samples(&held, 30.0, 0.0).is_ok());
        let over: Vec<(u64, Option<u32>)> = vec![(0, Some(10)), (1, Some(12)), (62, Some(12))];
        assert!(run_tick_samples(&over, 30.0, 0.0).is_err());
        // exactly RESTART_GAP_S (60 frames at 30 fps) is still a repeat: the bound is exclusive
        let at_bound: Vec<(u64, Option<u32>)> = vec![(0, Some(10)), (60, Some(10))];
        assert!(run_tick_samples(&at_bound, 30.0, 0.0).is_ok());
        // the gap is in seconds of the file's own fps: 60 frames at 25 fps is 2.4 s, a restart
        assert!(run_tick_samples(&at_bound, 25.0, 0.0).is_err());
    }

    #[test]
    fn a_bad_fps_is_an_error() {
        for fps in [0.0, -30.0, f64::NAN, f64::INFINITY] {
            assert!(run_tick_samples(&[(0, Some(0))], fps, 0.0).is_err());
        }
    }
}
