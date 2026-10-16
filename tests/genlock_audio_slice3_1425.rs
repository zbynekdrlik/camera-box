//! Issue 1425 (E2 slice 3, design 6220191978) — the 85 ms audio-buffering floor is 0 and the
//! absolute ASRC level target is gone, in the C and in its Rust mirror.
//!
//! Since slice 1 every directly stamped mixed source is judged by its own stamp, so the floor no
//! longer buys the 70 ms reach it was sized for, and since slice 2 the buffering cannot grow while
//! an audio encoder runs. The absolute 100 ms target then only walked an arrival-basis source's
//! buffer after its lock, a runtime latency change the owner's 16.10.2026 rule forbids ("raz keď sa
//! niečo zarovná a syncne, tak už to musí držať"). So:
//!
//! - the floor define and `FLOOR_MS` are 0, the reset log names it as a dynamic start;
//! - the absolute target, its offset store, its rule switch and its UNREACHABLE fallback (a runtime
//!   re-latch of the setpoint) are gone from asrc-compensator.{h,c}, obs-source.c, obs-audio.c and
//!   `src/asrc_bench.rs`; the arrival basis captures the depth at lock, the placement bases 0;
//! - the floor-sized band (its error, its reach, its nominal base) is gone with the target it served;
//! - the #786 launch gates no longer call a clean draw 85 ms, nor a high draw an A/V fault.
//!
//! The behaviour is pinned by the benches (`src/asrc_bench.rs`
//! `an_arrival_source_holds_its_depth_at_lock_and_never_walks_1425`, `src/asrc_stamp_bench.rs`
//! `the_arrival_servo_keeps_the_av_it_locked_at_1425`); this file pins the TEXT.
//!
//! Std-only: `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_audio_slice3_1425.rs`.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const BUFFERING_H: &str = "vendor/obs-studio/libobs/obs-genlock-audio-buffering.h";
const OBS_C: &str = "vendor/obs-studio/libobs/obs.c";
const OBS_AUDIO: &str = "vendor/obs-studio/libobs/obs-audio.c";
const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const ASRC_H: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.h";
const ASRC_C: &str = "vendor/obs-studio/libobs/media-io/asrc-compensator.c";
const BUFFERING_RS: &str = "src/genlock_audio_buffering.rs";
const ASRC_RS: &str = "src/asrc_bench.rs";
const GUARDED_LAUNCH: &str = "scripts/obs-guarded-launch.ps1";
const LAUNCH_GENLOCK: &str = "scripts/launch-obs-genlock.sh";
const RIG_HEALTH: &str = "scripts/rig-health-audit.py";

/// `(file, squished needle)`, each required exactly once.
const SLICE3_PRESENT: [(&str, &str); 6] = [
    (BUFFERING_H, "#define GENLOCK_AUDIO_BUFFERING_FLOOR_MS 0u"),
    (BUFFERING_RS, "pub const FLOOR_MS: u32 = 0;"),
    (
        ASRC_C,
        "if (c->timecode) c->level_target_ms = 0.0; else c->level_target_ms = window_level_ms;",
    ),
    (
        OBS_C,
        "\"\\tbuffering type: dynamically increasing from %d ms (genlock floor, issue 1425)\",",
    ),
    (
        OBS_AUDIO,
        "\"genlock audio buffering ABOVE the floor (issue 1367): adding %d milliseconds of audio buffering, total \" \"audio buffering is now %d milliseconds (source: %s)\",",
    ),
    (GUARDED_LAUNCH, "(norma pod 100 ms)"),
];

/// `(file, needle)`, each must be GONE.
const SLICE3_ABSENT: [(&str, &str); 29] = [
    // the absolute target, its offset store and its rule switch
    (ASRC_H, "ASRC_LEVEL_TARGET_MS"),
    (ASRC_H, "level_absolute"),
    (ASRC_H, "level_offset_ms"),
    (ASRC_C, "ASRC_LEVEL_TARGET_MS"),
    (ASRC_C, "level_absolute"),
    (ASRC_C, "level_offset_ms"),
    (OBS_SOURCE, "asrc_compensator_set_level_absolute"),
    (OBS_SOURCE, "asrc_compensator_set_level_offset_ms"),
    (OBS_AUDIO, "ASRC_LEVEL_TARGET_MS"),
    (ASRC_RS, "LEVEL_TARGET_MS"),
    (ASRC_RS, "level_absolute"),
    (ASRC_RS, "level_offset_ms"),
    // the UNREACHABLE fallback: a runtime re-latch of the setpoint
    (ASRC_H, "UNREACHABLE"),
    (ASRC_H, "level_fallback"),
    (ASRC_C, "UNREACHABLE"),
    (ASRC_C, "level_fallback"),
    (OBS_SOURCE, "UNREACHABLE"),
    (OBS_SOURCE, "level_fallback"),
    (OBS_SOURCE, "fallbacks=%u"),
    (ASRC_RS, "level_fallback"),
    (ASRC_RS, "LEVEL_TARGET_UNREACHABLE_WINDOWS"),
    // the floor-sized band the absolute target needed
    (BUFFERING_H, "GENLOCK_AUDIO_LEVEL_REACH_NS"),
    (BUFFERING_H, "band_error_ns"),
    (OBS_AUDIO, "ASRC level band"),
    (BUFFERING_RS, "LEVEL_REACH_NS"),
    (BUFFERING_RS, "band_error_ns"),
    // the #786 launch gates: 85 ms is no longer the norm, a high draw is not an A/V fault
    (GUARDED_LAUNCH, "MIMO"),
    (GUARDED_LAUNCH, "norma 85"),
    (LAUNCH_GENLOCK, "85 ms floor"),
];

#[test]
fn the_floor_is_zero_and_the_absolute_target_is_gone_1425() {
    for (file, needle) in SLICE3_PRESENT {
        let n = squish(&read(file)).matches(needle).count();
        assert_eq!(
            n, 1,
            "issue 1425 slice 3: {file} must carry `{needle}` exactly once, found {n}"
        );
    }
    for (file, needle) in SLICE3_ABSENT {
        assert!(
            !read(file).contains(needle),
            "issue 1425 slice 3: {file} must no longer carry `{needle}`"
        );
    }
}

#[test]
fn the_launch_gates_keep_their_bound_and_drop_the_85_ms_norm_1425() {
    // The 100 ms bound stays: a draw above it still redraws. Only the norm text moved: a clean
    // launch draws only what its ASIO / PulseAudio startup race adds (stock OBS drew 0, 64 or 85 ms
    // per launch) or no growth line.
    let py = read(RIG_HEALTH);
    assert!(
        py.contains("AUDIO_BUF_BOUND_MS = 100") && !py.contains("85 ms floor"),
        "issue 1425 slice 3: {RIG_HEALTH} keeps the 100 ms bound and drops the 85 ms floor norm"
    );
    let sh = read(LAUNCH_GENLOCK);
    assert!(
        sh.contains("stock OBS drew") && sh.contains("0, 64 or 85 ms per launch"),
        "issue 1425 slice 3: {LAUNCH_GENLOCK} (3b) names the stock clean draws (0, 64 or 85 ms)"
    );
    let ps = read(GUARDED_LAUNCH);
    assert!(
        ps.contains("$threshold  = 100") && ps.matches("norma pod 100 ms)").count() == 2,
        "issue 1425 slice 3: {GUARDED_LAUNCH} keeps the 100 ms bound and both popups name it as the norm"
    );
}
