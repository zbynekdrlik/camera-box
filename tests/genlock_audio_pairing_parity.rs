//! #1303 + issue 1367 — an EXECUTABLE C-vs-Rust parity gate for the receiver-side audio-pairing
//! decisions.
//!
//! `src/genlock_audio_pairing.rs` is the Tier-0 authority; the contiguous `genlock_audio_*` /
//! `genlock_video_delay_*` `static inline` block in `vendor/obs-studio/libobs/obs-source.c` is the
//! production port the audio ingest (`source_output_audio_data`), the render-thread video-delay
//! tracker, the `genlock-fifo audit` audio facet and the LOCK indicator consume. The two are
//! required to be numerically identical. A static text anchor (`tests/genlock_preload.rs`) proves
//! the C still *says* the right thing, but libobs is compiled only by the genlock workflows, so
//! nothing else executes it.
//!
//! This gate closes that hole: it lifts the contiguous block VERBATIM out of `obs-source.c`,
//! compiles it standalone with `cc` under `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, runs the
//! C decisions over a spread of vectors, and requires byte-identical results from
//! [`camera_box::genlock_audio_pairing`] on the same inputs. A divergence — a flipped precedence,
//! a wrong truncation, a wrap in the placement arithmetic, a tracker that applies mid-step — fails
//! here in seconds instead of surviving to a live rig.
//!
//! Per the project's test-strictness rule this FAILS LOUDLY rather than skipping if the C
//! toolchain is missing — a parity test that silently passes without running is worse than none.

use camera_box::genlock_audio_pairing::{
    audio_actual_place_ns, audio_place_error_ns, audio_place_error_smooth_ns,
    audio_push_back_allowed, audio_realized_delay_ns,
};
use camera_box::genlock_audio_pairing::{
    audio_applied_delay_ns, audio_hold_action, audio_hold_mode, audio_hold_ms,
    audio_level_shift_ns, audio_needs_live_offset, audio_place_term_ns, audio_placed_slew_fold_ns,
    audio_slew_book_ts_ns, audio_slew_ppm, audio_slew_step_ns, audio_wall_to_mono_ns,
    audio_withhold_expired, decide_audio_health, genlock_audio_delay_ns, pairing_offset_ms,
    video_delay_lock_ms, video_delay_moved, video_delay_reference_ns, video_delay_round_ms,
    video_delay_sample_ns, video_delay_smooth_ns, video_delay_track, AudioHoldAction,
    AudioHoldMode, AudioPairingFacets, VideoDelayTracker, VIDEO_DELAY_LOCK_PENDING,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SRC: &str = "vendor/obs-studio/libobs/obs-source.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Lift the contiguous block VERBATIM out of obs-source.c: from the `genlock_audio_present_delay_ns`
/// signature through `genlock_audio_decide_health`'s closing brace.
fn lift_block() -> String {
    let path = repo(SRC);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline uint64_t genlock_audio_present_delay_ns(")
        .unwrap_or_else(|| {
            panic!("#1303: {SRC} no longer defines `genlock_audio_present_delay_ns` — the pure audio-pairing block is gone, nothing to check parity against.")
        });
    let last_fn = src
        .find("static inline int genlock_audio_decide_health(")
        .unwrap_or_else(|| panic!("#1303: {SRC} no longer defines `genlock_audio_decide_health`"));
    assert!(
        last_fn > start,
        "#1303: the genlock_audio_* helpers are no longer contiguous in {SRC} — keep the block together or the lift splices unrelated code."
    );
    let end = src[last_fn..]
        .find("\n}\n")
        .map(|i| last_fn + i + 3)
        .expect("#1303: genlock_audio_decide_health has no closing brace");
    let block = src[start..end].to_string();
    for helper in [
        "genlock_video_delay_sample_ns(",
        "genlock_video_delay_smooth_ns(",
        "genlock_video_delay_round_ms(",
        "genlock_video_delay_moved(",
        "genlock_video_delay_track(",
        "genlock_audio_hold_mode(",
        "genlock_audio_hold_ms(",
        "genlock_audio_hold_token(",
        "genlock_audio_needs_live_offset(",
        "genlock_audio_wall_to_mono_ns(",
        "genlock_audio_place_term_ns(",
        "genlock_audio_video_delay_ref_ns(",
        "genlock_audio_pairing_offset_ms(",
        "genlock_video_delay_lock_ms(",
        "genlock_audio_withhold_expired(",
        "genlock_audio_mode_active(",
        "genlock_audio_hold_action(",
        "genlock_audio_level_shift_ns(",
        "genlock_audio_slew_step_ns(",
        "genlock_audio_slew_ppm(",
        "genlock_audio_slew_book_ts_ns(",
        "genlock_audio_placed_slew_fold_ns(",
        "genlock_audio_applied_delay_ns(",
        "genlock_audio_push_back_allowed(",
        "genlock_audio_actual_place_ns(",
        "genlock_audio_place_error_ns(",
        "genlock_audio_place_error_smooth_ns(",
        "genlock_audio_realized_delay_ns(",
    ] {
        assert!(
            block.contains(helper),
            "issue 1367: `{helper}` is no longer inside the contiguous audio-pairing block of {SRC}"
        );
    }
    block
}

fn compile(block: &str, main_body: &str, tag: &str) -> PathBuf {
    let mut c = String::from("#include <stdint.h>\n#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(block);
    c.push_str("int main(void){\n");
    c.push_str(main_body);
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_audio_pairing_parity_1303");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join(format!("{tag}.c"));
    let bin = dir.join(format!("{tag}.bin"));
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1303: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 audio-pairing helpers to prove the C and the Rust authority agree numerically; it \
                 must FAIL rather than skip when the toolchain is absent. Install a C compiler or \
                 set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1303: the audio-pairing helpers lifted from {SRC} do NOT COMPILE standalone under \
         -Wall -Wextra -Wconversion -Wformat=2 -Werror. libobs is otherwise built only by the \
         genlock workflows, so this is very likely a real compile error heading for CI:\n--- cc \
         stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

fn run_lines(bin: &PathBuf) -> Vec<String> {
    let run = Command::new(bin)
        .output()
        .expect("#1303: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1303: the parity harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn run_c(body: &str, tag: &str) -> Vec<String> {
    run_lines(&compile(&lift_block(), body, tag))
}

const IV30: u64 = 33_333_333;
const IV60: u64 = 16_666_666;
const WALL: u64 = 1_790_000_000_123_456_789;
const MONO: u64 = 86_400_000_000_000;

#[test]
fn c_audio_delay_matches_the_rust_authority_1303() {
    let holds: [u32; 10] = [0, 1, 3, 8, 13, 20, 33, 100, 923, 2000];
    let mut body = String::new();
    for &l in &holds {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_audio_present_delay_ns({l}u));\n"
        ));
    }
    let out = run_c(&body, "delay");
    assert_eq!(out.len(), holds.len());
    for (l, got) in holds.iter().zip(&out) {
        let c: u64 = got.trim().parse().expect("delay u64");
        assert_eq!(
            c,
            genlock_audio_delay_ns(*l),
            "#1303: genlock_audio_present_delay_ns({l})"
        );
    }
}

#[test]
fn c_video_delay_scalar_helpers_match_the_rust_authority_1367() {
    let samples: [(u64, u64); 9] = [
        (WALL, WALL - 97_000_000),
        (WALL, WALL),
        (WALL - 5, WALL),
        (1_000_100_000_000, 1_000_000_000_000),
        (u64::MAX, 0),
        (0, u64::MAX),
        (WALL, WALL + 1),
        (WALL + 1, WALL),
        (WALL + 2, WALL),
    ];
    let smooths: [(u64, u64); 12] = [
        (0, 97_000_000),
        (100_000_000, 108_000_000),
        (100_000_000, 92_000_000),
        (100, 107),
        (100, 93),
        (100_000_000, 133_333_333),
        (66_666_667, 100_000_000),
        (1, 0),
        (1, u64::MAX),
        (u64::MAX, 1),
        (9_223_372_036_854_775_814, 3),
        (9_223_372_036_854_775_818, 9_223_372_036_854_775_807),
    ];
    let rounds: [u64; 7] = [
        0,
        400_000,
        66_499_999,
        66_500_000,
        66_666_666,
        4_294_967_295_600_000,
        u64::MAX,
    ];
    let moves: [(u32, u64, u64); 10] = [
        (0, 100_000_000, IV30),
        (100, 116_666_666, IV30),
        (100, 116_666_667, IV30),
        (100, 83_333_333, IV30),
        (100, 83_333_334, IV30),
        (50, 58_333_333, IV60),
        (50, 58_333_332, IV60),
        (50, 900_000_000, 0),
        (0, 0, 0),
        (1, u64::MAX, IV30),
    ];
    let mut body = String::new();
    for (t, h) in &samples {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_video_delay_sample_ns({t}ull, {h}ull));\n"
        ));
    }
    for (s, x) in &smooths {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_video_delay_smooth_ns({s}ull, {x}ull));\n"
        ));
    }
    for r in &rounds {
        body.push_str(&format!(
            "    printf(\"%u\\n\", (unsigned)genlock_video_delay_round_ms({r}ull));\n"
        ));
    }
    for (a, s, iv) in &moves {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_video_delay_moved({a}u, {s}ull, {iv}ull) ? 1 : 0);\n"
        ));
    }
    let out = run_c(&body, "video_scalars");
    let mut want: Vec<String> = Vec::new();
    for (t, h) in &samples {
        want.push(video_delay_sample_ns(*t, *h).to_string());
    }
    for (s, x) in &smooths {
        want.push(video_delay_smooth_ns(*s, *x).to_string());
    }
    for r in &rounds {
        want.push(video_delay_round_ms(*r).to_string());
    }
    for (a, s, iv) in &moves {
        want.push((video_delay_moved(*a, *s, *iv) as i32).to_string());
    }
    assert_eq!(
        out, want,
        "issue 1367: a C video-delay helper diverged from the Rust authority"
    );
}

/// Drive the C and the Rust tracker through the same per-tick sample sequences (first seed, a
/// one-frame step each way, single held ticks, a reversed transient, 60 fps) and compare the full
/// state after EVERY tick.
#[test]
fn c_video_delay_tracker_matches_the_rust_authority_tick_by_tick_1367() {
    // (sample, interval, lock_ms): the free tracker, then (issue 1367, ROZHODNUTÉ 5827497952) a
    // pending window, a latched lock that ignores the floating samples, and a release back to free.
    let mut seq: Vec<(u64, u64, u32)> = Vec::new();
    seq.extend(std::iter::repeat_n((100_000_000, IV30, 0), 120));
    seq.extend(std::iter::repeat_n((66_666_667, IV30, 0), 150));
    for i in 0..200u64 {
        seq.push((if i % 10 == 0 { 100_000_000 } else { 66_666_667 }, IV30, 0));
    }
    // a 20-tick excursion one frame deeper arms the settle; it comes back to 70 ms, 3 ms off the
    // applied 67 — under half a frame, so the settle must apply NOTHING.
    seq.extend(std::iter::repeat_n((100_000_000, IV30, 0), 20));
    seq.extend(std::iter::repeat_n((70_000_000, IV30, 0), 100));
    seq.extend(std::iter::repeat_n((133_333_333, IV30, 0), 150));
    seq.extend(std::iter::repeat_n((50_000_000, IV60, 0), 120));
    seq.extend(std::iter::repeat_n((58_400_000, IV60, 0), 120));
    // a pending window mid-arm (the settle must clear, nothing applied), then a lock of 100 ms over
    // floating samples, a relock to 67, and the free tracker again.
    seq.extend(std::iter::repeat_n((133_333_333, IV30, 0), 5));
    seq.extend(std::iter::repeat_n(
        (66_666_667, IV30, VIDEO_DELAY_LOCK_PENDING),
        40,
    ));
    for i in 0..150u64 {
        seq.push((
            [66_666_667, 100_000_000, 133_333_333][(i / 25 % 3) as usize],
            IV30,
            100,
        ));
    }
    seq.extend(std::iter::repeat_n((66_666_667, IV30, 67), 30));
    seq.extend(std::iter::repeat_n((100_000_000, IV30, 0), 150));
    // design 5830750134: a lock of 400 ms over a realized ~233 ms is followed once the offset has
    // held for VIDEO_DELAY_FOLLOW_TICKS (with a short return to 400 that resets the count), the
    // same lock over a realized 500 ms follows upward, a NEW lock applies at once, and the free
    // tracker afterwards starts from a fresh arm.
    seq.extend(std::iter::repeat_n((233_333_333, IV30, 400), 100));
    seq.extend(std::iter::repeat_n((400_000_000, IV30, 400), 30));
    seq.extend(std::iter::repeat_n((233_333_333, IV30, 400), 250));
    seq.extend(std::iter::repeat_n((500_000_000, IV30, 400), 250));
    seq.extend(std::iter::repeat_n((100_000_000, IV30, 100), 60));
    seq.extend(std::iter::repeat_n((233_333_333, IV30, 0), 100));
    // a lock left MID follow-count: the free tracker must start from a fresh arm, not the count.
    seq.extend(std::iter::repeat_n((100_000_000, IV30, 100), 5));
    seq.extend(std::iter::repeat_n((200_000_000, IV30, 100), 60));
    seq.extend(std::iter::repeat_n((200_000_000, IV30, 0), 80));

    let mut body = String::from(
        "    uint64_t sm = 0; uint32_t ap = 0; uint32_t st = 0; uint32_t lk = 0;\n    static const unsigned long long S[] = {",
    );
    body.push_str(
        &seq.iter()
            .map(|(s, _, _)| format!("{s}ull"))
            .collect::<Vec<_>>()
            .join(","),
    );
    body.push_str("};\n    static const unsigned long long I[] = {");
    body.push_str(
        &seq.iter()
            .map(|(_, i, _)| format!("{i}ull"))
            .collect::<Vec<_>>()
            .join(","),
    );
    body.push_str("};\n    static const unsigned int L[] = {");
    body.push_str(
        &seq.iter()
            .map(|(_, _, l)| format!("{l}u"))
            .collect::<Vec<_>>()
            .join(","),
    );
    body.push_str(&format!(
        "}};\n    for (int k = 0; k < {}; k++) {{\n        genlock_video_delay_track(&sm, &ap, &st, &lk, L[k], S[k], I[k]);\n        printf(\"%llu %u %u %u\\n\", (unsigned long long)sm, (unsigned)ap, (unsigned)st, (unsigned)lk);\n    }}\n",
        seq.len()
    ));
    let out = run_c(&body, "tracker");
    let mut t = VideoDelayTracker::default();
    let mut want = Vec::new();
    for (s, iv, lock) in &seq {
        video_delay_track(&mut t, *lock, *s, *iv);
        want.push(format!(
            "{} {} {} {}",
            t.smoothed_ns, t.applied_ms, t.settle_ticks, t.locked_ms
        ));
    }
    assert_eq!(out.len(), want.len());
    let diffs: Vec<String> = out
        .iter()
        .zip(&want)
        .enumerate()
        .filter(|(_, (c, r))| c != r)
        .map(|(k, (c, r))| format!("  tick {k}: C `{c}` vs Rust `{r}`"))
        .take(10)
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: the C genlock_video_delay_track diverged from the Rust authority:\n{}",
        diffs.join("\n")
    );
    // the sequence must actually exercise re-applications, or the gate compares idle trackers.
    // A line is `smoothed applied settle locked`.
    let seen = |applied: u32, locked: u32| {
        want.iter().any(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            f[1] == applied.to_string() && f[2] == "0" && f[3] == locked.to_string()
        })
    };
    assert!(seen(67, 0), "the sequence never applied 67 ms");
    assert!(seen(133, 0), "the sequence never applied 133 ms");
    assert!(
        !seen(70, 0),
        "the reversed excursion must not apply its sub-half-frame 70 ms"
    );
    assert!(seen(100, 100), "the sequence never applied the 100 ms lock");
    // design 5830750134: the 400 ms lock was bounded by the realized 233 ms, then followed 500 ms.
    assert!(
        seen(233, 400),
        "the lock never followed the realized 233 ms"
    );
    assert!(
        seen(500, 400),
        "the lock never followed the realized 500 ms"
    );
}

#[test]
fn c_audio_hold_and_placement_match_the_rust_authority_1367() {
    let modes: [(bool, u32, bool, u32, bool); 11] = [
        (false, 3, true, 97, false),
        (true, 0, true, 97, false),
        (true, 3, true, 0, false),
        (true, 3, true, 0, true),
        (true, 3, false, 97, false),
        (true, 3, true, 97, false),
        (true, 3, true, 97, true),
        (true, 923, true, 1033, false),
        (true, 3, false, 0, false),
        (true, 3, false, 0, true),
        (false, 3, true, 0, false),
    ];
    let offs: [(u64, u64); 4] = [
        (MONO, WALL),
        (WALL + 5, WALL),
        (WALL, WALL + 5),
        (0, u64::MAX),
    ];
    let off_live = audio_wall_to_mono_ns(MONO, WALL);
    let ta = MONO.wrapping_sub(WALL - 2_500_000);
    let terms: [(u8, u32, i64, u64); 7] = [
        (0, 97, off_live, ta),
        (1, 3, off_live, ta),
        (2, 97, off_live, ta),
        (2, 67, off_live + 300_000_000, ta),
        (2, 1033, i64::MIN + 7, u64::MAX),
        (2, 0, 0, 0),
        (1, 923, i64::MAX, 0),
    ];
    let mut body = String::new();
    for (g, l, w, v, x) in &modes {
        body.push_str(&format!(
            "    {{ int m = genlock_audio_hold_mode({}, {l}u, {}, {v}u, {}); printf(\"%d %u %s\\n\", m, (unsigned)genlock_audio_hold_ms(m, {l}u, {v}u), genlock_audio_hold_token(m)); }}\n",
            *g as i32, *w as i32, *x as i32
        ));
    }
    for (m, w) in &offs {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_wall_to_mono_ns({m}ull, {w}ull));\n"
        ));
    }
    for (m, h, o, t) in &terms {
        // i64::MIN has no literal form in C; build it from (-MAX - 1).
        let o_lit = if *o == i64::MIN + 7 {
            "(INT64_MIN + 7)".to_string()
        } else {
            format!("{o}ll")
        };
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_place_term_ns({m}, {h}u, {o_lit}, {t}ull));\n"
        ));
    }
    for m in 0u8..4 {
        for p in 0u8..4 {
            body.push_str(&format!(
                "    printf(\"%d\\n\", genlock_audio_needs_live_offset({m}, {p}) ? 1 : 0);\n"
            ));
        }
    }
    // the pending mode places nothing.
    body.push_str(&format!(
        "    printf(\"%lld\\n\", (long long)genlock_audio_place_term_ns(3, 97u, {off_live}ll, {ta}ull));\n"
    ));
    let out = run_c(&body, "hold_place");
    let mut want: Vec<String> = Vec::new();
    for (g, l, w, v, x) in &modes {
        let m = audio_hold_mode(*g, *l, *w, *v, *x);
        want.push(format!(
            "{} {} {}",
            m.code(),
            audio_hold_ms(m, *l, *v),
            m.token()
        ));
    }
    for (m, w) in &offs {
        want.push(audio_wall_to_mono_ns(*m, *w).to_string());
    }
    for (m, h, o, t) in &terms {
        want.push(audio_place_term_ns(mode_of(*m), *h, *o, *t).to_string());
    }
    for m in 0u8..4 {
        for p in 0u8..4 {
            want.push((audio_needs_live_offset(mode_of(m), mode_of(p)) as i32).to_string());
        }
    }
    want.push(audio_place_term_ns(AudioHoldMode::Pending, 97, off_live, ta).to_string());
    assert_eq!(
        out, want,
        "issue 1367: a C audio hold/placement helper diverged from the Rust authority"
    );
}

fn mode_of(c: u8) -> AudioHoldMode {
    match c {
        0 => AudioHoldMode::Off,
        1 => AudioHoldMode::Latency,
        2 => AudioHoldMode::Timecode,
        _ => AudioHoldMode::Pending,
    }
}

fn i64_lit(v: i64) -> String {
    if v == i64::MIN {
        "INT64_MIN".to_string()
    } else {
        format!("{v}ll")
    }
}

/// Issue 1367 (ROZHODNUTÉ 5827497952): the locked tracker input, the withhold clock, the per-packet
/// action (an exhaustive sweep of every mode pair x hold-changed x continuous x can_slew x
/// slew_pending), the level shift and the slew step / ppm.
#[test]
fn c_withhold_action_and_slew_match_the_rust_authority_1367() {
    let locks: [(u64, bool, u64); 8] = [
        (2, false, IV30),
        (3, true, IV30),
        (2, false, IV60),
        (0, true, IV30),
        (0, false, IV30),
        (2, true, 0),
        (u64::MAX, false, IV30),
        (1_000_000_000_000, false, IV30),
    ];
    let expiries: [(u64, u64); 6] = [
        (0, u64::MAX),
        (5_000_000_000, 5_000_000_000),
        (5_000_000_000, 14_999_999_999),
        (5_000_000_000, 15_000_000_000),
        (5_000_000_000, 1),
        (1, u64::MAX),
    ];
    let mut actions = Vec::new();
    for pm in 0u8..4 {
        for m in 0u8..4 {
            for changed_hold in [false, true] {
                for cont in [false, true] {
                    for slew in [false, true] {
                        for pend in [false, true] {
                            actions.push((pm, m, changed_hold, cont, slew, pend));
                        }
                    }
                }
            }
        }
    }
    let shifts: [(u8, u8, i64, i64, i64); 8] = [
        (1, 2, 67_000_000, 100_000_000, 10_000_000),
        (4, 2, 67_000_000, 100_000_000, 0),
        (1, 3, i64::MIN, 0, 0),
        (1, 0, 5, 0, 0),
        (3, 2, 67, 100, 0),
        (2, 2, 67, 100, 9),
        (1, 1, i64::MAX, -1, 1),
        (0, 2, 1, 2, 3),
    ];
    let steps: [(i64, u64); 9] = [
        (33_000_000, 10_666_666),
        (-33_000_000, 10_666_666),
        (4_000, 10_666_666),
        (0, 10_666_666),
        (i64::MIN, u64::MAX),
        (i64::MAX, u64::MAX),
        (7, 0),
        (-10_666, 10_666_666),
        (-10_667, 10_666_666),
    ];
    let mut body = String::new();
    for (t, m, iv) in &locks {
        body.push_str(&format!(
            "    printf(\"%u\\n\", (unsigned)genlock_video_delay_lock_ms({t}ull, {}, {iv}ull));\n",
            *m as i32
        ));
    }
    for (f, n) in &expiries {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_withhold_expired({f}ull, {n}ull) ? 1 : 0);\n"
        ));
    }
    for (pm, m, ch, cont, slew, pend) in &actions {
        let hold = if *ch { 67 } else { 100 };
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_hold_action({pm}, 100u, {m}, {hold}u, {}, {}, {}));\n",
            *cont as i32, *slew as i32, *pend as i32
        ));
    }
    for (a, pm, n, p, r) in &shifts {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_level_shift_ns({a}, {pm}, {}, {}, {}));\n",
            i64_lit(*n),
            i64_lit(*p),
            i64_lit(*r)
        ));
    }
    for (r, dt) in &steps {
        body.push_str(&format!(
            "    printf(\"%lld %.6f\\n\", (long long)genlock_audio_slew_step_ns({}, {dt}ull), genlock_audio_slew_ppm(genlock_audio_slew_step_ns({}, {dt}ull), {dt}ull));\n",
            i64_lit(*r),
            i64_lit(*r)
        ));
    }
    // the booking of a slew step out of the smoothing timeline (wrapping both ways) and the fold of
    // an owed slew into the level setpoint when the ingest placed the packet anyway.
    let books: [(u64, i64); 6] = [
        (1_000, 5),
        (1_000, -5),
        (3, 10),
        (u64::MAX, -1),
        (0, 0),
        (5_000_000_000, 1_000_000),
    ];
    let mut folds = Vec::new();
    for a in 0..5u8 {
        for placed in [false, true] {
            for r in [0i64, 33_000_000, -12] {
                folds.push((a, placed, r));
            }
        }
    }
    for (t, st) in &books {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_audio_slew_book_ts_ns({t}ull, {}));\n",
            i64_lit(*st)
        ));
    }
    for (a, placed, r) in &folds {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_placed_slew_fold_ns({a}, {}, {}));\n",
            *placed as i32,
            i64_lit(*r)
        ));
    }
    let out = run_c(&body, "withhold_action_slew");
    let action_of = |c: u8| match c {
        0 => AudioHoldAction::Withhold,
        1 => AudioHoldAction::Place,
        2 => AudioHoldAction::Continue,
        3 => AudioHoldAction::Slew,
        _ => AudioHoldAction::Replace,
    };
    let mut want: Vec<String> = Vec::new();
    for (t, m, iv) in &locks {
        want.push(video_delay_lock_ms(*t, *m, *iv).to_string());
    }
    for (f, n) in &expiries {
        want.push((audio_withhold_expired(*f, *n) as i32).to_string());
    }
    let mut seen = std::collections::BTreeSet::new();
    for (pm, m, ch, cont, slew, pend) in &actions {
        let hold = if *ch { 67 } else { 100 };
        let a = audio_hold_action(mode_of(*pm), 100, mode_of(*m), hold, *cont, *slew, *pend);
        seen.insert(a.code());
        want.push(a.code().to_string());
    }
    assert_eq!(seen.len(), 5, "the sweep must reach every action: {seen:?}");
    for (a, pm, n, p, r) in &shifts {
        want.push(audio_level_shift_ns(action_of(*a), mode_of(*pm), *n, *p, *r).to_string());
    }
    for (r, dt) in &steps {
        let st = audio_slew_step_ns(*r, *dt);
        want.push(format!("{st} {:.6}", audio_slew_ppm(st, *dt)));
    }
    for (t, st) in &books {
        want.push(audio_slew_book_ts_ns(*t, *st).to_string());
    }
    let mut folded = 0usize;
    for (a, placed, r) in &folds {
        let f = audio_placed_slew_fold_ns(action_of(*a), *placed, *r);
        folded += usize::from(f != 0);
        want.push(f.to_string());
    }
    assert!(
        folded > 0 && folded < folds.len(),
        "the fold vectors must reach both outcomes: {folded}"
    );
    assert_eq!(
        out, want,
        "issue 1367: a C withhold / action / slew helper diverged from the Rust authority"
    );
}

#[test]
fn c_pairing_offset_matches_the_rust_authority_1367() {
    let refs: [(u64, u32); 4] = [(97_400_000, 3), (0, 3), (0, 923), (1_033_000_000, 987)];
    let cases: [(i64, i64); 9] = [
        (3_000_000, 97_000_000),
        (97_000_000, 97_400_000),
        (100_000_000, 83_400_000),
        (67_000_000, 83_700_000),
        (0, 97_000_000),
        (923_000_000, 923_000_000),
        (16_000_000, 16_666_666),
        (i64::MAX, -1),
        (-1_500_000, 0),
    ];
    let mut body = String::new();
    for (s, l) in &refs {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_video_delay_ref_ns({s}ull, {l}u));\n"
        ));
    }
    for (a, v) in &cases {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_pairing_offset_ms({a}ll, {v}ll));\n"
        ));
    }
    // review round 2: the audio side of the offset is the hold minus the slew still owed.
    let applied: [(u32, i64); 6] = [
        (100, 0),
        (100, 33_000_000),
        (67, -33_000_000),
        (0, 5),
        (u32::MAX, i64::MIN),
        (3, i64::MAX),
    ];
    for (h, r) in &applied {
        let r_lit = if *r == i64::MIN {
            "INT64_MIN".to_string()
        } else {
            format!("{r}ll")
        };
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_applied_delay_ns({h}u, {r_lit}));\n"
        ));
    }
    let out = run_c(&body, "offset");
    let mut want: Vec<String> = Vec::new();
    for (s, l) in &refs {
        want.push(video_delay_reference_ns(*s, *l).to_string());
    }
    for (a, v) in &cases {
        want.push(pairing_offset_ms(*a, *v).to_string());
    }
    for (h, r) in &applied {
        want.push(audio_applied_delay_ns(*h, *r).to_string());
    }
    // a slew still owing 33 ms of a 100 ms hold against a 100 ms video reads -33, not 0.
    assert_eq!(
        pairing_offset_ms(audio_applied_delay_ns(100, 33_000_000), 100_000_000),
        -33
    );
    assert_eq!(
        out, want,
        "issue 1367: the C pairing offset diverged from the Rust authority"
    );
}

#[test]
fn c_audio_health_matches_the_rust_authority_1303() {
    // Exhaustive sweep of the three booleans crossed with a set of (pairing_offset, frame_interval)
    // pairs spanning below / at / above the HALF-frame bound in both signs, at 30 and 60 fps, plus
    // the i64 extremes.
    let off_intervals: [(i64, i64); 14] = [
        (0, 33),
        (16, 33),
        (17, 33),
        (-16, 33),
        (-17, 33),
        (33, 33),
        (-94, 33),
        (8, 16),
        (9, 16),
        (-9, 16),
        (999, 33),
        (i64::MIN, 33),
        (i64::MAX, i64::MAX),
        (5, 0),
    ];
    let mut vectors = Vec::new();
    for audio_enabled in [false, true] {
        for is_program in [false, true] {
            for asrc_sat in [false, true] {
                for &(off, iv) in &off_intervals {
                    vectors.push((audio_enabled, is_program, asrc_sat, off, iv));
                }
            }
        }
    }
    let lit = |v: i64| {
        if v == i64::MIN {
            "INT64_MIN".to_string()
        } else {
            format!("{v}ll")
        }
    };
    let mut body = String::new();
    for (ae, ip, sat, off, iv) in &vectors {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_decide_health({}, {}, {}, {}, {}));\n",
            *ae as i32,
            *ip as i32,
            *sat as i32,
            lit(*off),
            lit(*iv)
        ));
    }
    let out = run_c(&body, "health");
    assert_eq!(out.len(), vectors.len());
    let mut diffs = Vec::new();
    for ((ae, ip, sat, off, iv), got) in vectors.iter().zip(&out) {
        let c: u8 = got.trim().parse().expect("health code");
        let rs = decide_audio_health(&AudioPairingFacets {
            audio_enabled: *ae,
            is_program_source: *ip,
            asrc_saturated: *sat,
            pairing_offset_ms: *off,
            frame_interval_ms: *iv,
        })
        .code();
        if c != rs {
            diffs.push(format!(
                "  audio_enabled={ae} is_program={ip} asrc_sat={sat} off={off} iv={iv} -> C {c}, Rust {rs}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1303: the vendored C genlock_audio_decide_health DIVERGED from the Tier-0 Rust authority \
         on {} of {} vectors:\n{}",
        diffs.len(),
        vectors.len(),
        diffs.join("\n")
    );
}

/// Issue 1367 (live 25.9.2026 12:31) — the append-after-reset guard and the placement measurement
/// the audit's pairing offset now reads: `genlock_audio_push_back_allowed`,
/// `genlock_audio_actual_place_ns`, `genlock_audio_place_error_ns`,
/// `genlock_audio_place_error_smooth_ns` and `genlock_audio_realized_delay_ns` must match the Rust
/// authority on every mode, both append paths, both error signs and the wrap extremes.
#[test]
fn c_audio_placement_measurement_matches_the_rust_authority_1367() {
    let modes = [
        AudioHoldMode::Off,
        AudioHoldMode::Latency,
        AudioHoldMode::Timecode,
        AudioHoldMode::Pending,
    ];
    let actual: [(bool, u64, u64, u64); 5] = [
        (true, 1_000, 250, 9_999),
        (false, 1_000, 250, 9_999),
        (true, u64::MAX, 2, 0),
        (false, 0, u64::MAX, u64::MAX),
        (true, 1_790_000_000_000_000_000, 144_000_000, 0),
    ];
    let errs: [(u64, u64); 5] = [
        (
            1_790_000_000_000_000_000 - 88_000_000,
            1_790_000_000_000_000_000,
        ),
        (5, 0),
        (0, 5),
        (0, u64::MAX),
        (u64::MAX, 0),
    ];
    let smooth: [(i64, i64, bool); 7] = [
        (123, -88_000_000, false),
        (0, 160, true),
        (0, -160, true),
        (0, -15, true),
        (i64::MIN, i64::MAX, true),
        (i64::MAX, i64::MIN, true),
        (-88_000_000, -87_000_000, true),
    ];
    let realized: [(u32, i64, i64, bool); 6] = [
        (133, 0, -88_000_000, true),
        (133, 33_000_000, -33_000_000, true),
        (133, 33_000_000, -88_000_000, false),
        (0, 0, 0, true),
        (u32::MAX, i64::MIN, i64::MAX, true),
        (3, i64::MAX, i64::MIN, false),
    ];
    let lit = |v: i64| {
        if v == i64::MIN {
            "INT64_MIN".to_string()
        } else {
            format!("{v}ll")
        }
    };
    let b = |v: bool| i32::from(v);
    let mut body = String::new();
    for m in &modes {
        for (pb, reset) in [(false, false), (true, false), (false, true), (true, true)] {
            body.push_str(&format!(
                "    printf(\"%d\\n\", genlock_audio_push_back_allowed({}, {}, {}) ? 1 : 0);\n",
                b(pb),
                b(reset),
                m.code()
            ));
        }
    }
    for (ap, ts, buf, placed) in &actual {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_audio_actual_place_ns({}, {ts}ull, {buf}ull, {placed}ull));\n",
            b(*ap)
        ));
    }
    for (a, i) in &errs {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_place_error_ns({a}ull, {i}ull));\n"
        ));
    }
    for (sm, sa, seeded) in &smooth {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_place_error_smooth_ns({}, {}, {}));\n",
            lit(*sm),
            lit(*sa),
            b(*seeded)
        ));
    }
    for (h, r, e, measured) in &realized {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_realized_delay_ns({h}u, {}, {}, {}));\n",
            lit(*r),
            lit(*e),
            b(*measured)
        ));
    }
    let out = run_c(&body, "placement");
    let mut want: Vec<String> = Vec::new();
    for m in &modes {
        for (pb, reset) in [(false, false), (true, false), (false, true), (true, true)] {
            want.push(b(audio_push_back_allowed(pb, reset, *m)).to_string());
        }
    }
    for (ap, ts, buf, placed) in &actual {
        want.push(audio_actual_place_ns(*ap, *ts, *buf, *placed).to_string());
    }
    for (a, i) in &errs {
        want.push(audio_place_error_ns(*a, *i).to_string());
    }
    for (sm, sa, seeded) in &smooth {
        want.push(audio_place_error_smooth_ns(*sm, *sa, *seeded).to_string());
    }
    for (h, r, e, measured) in &realized {
        want.push(audio_realized_delay_ns(*h, *r, *e, *measured).to_string());
    }
    assert_eq!(
        out, want,
        "issue 1367: the C append guard / placement measurement diverged from the Rust authority"
    );
    // the live 12:36 read must not read paired: 133 ms hold, samples 88 ms early.
    assert_eq!(
        pairing_offset_ms(
            audio_realized_delay_ns(133, 0, -88_000_000, true),
            133_333_333
        ),
        -88
    );
    assert!(!audio_push_back_allowed(
        true,
        true,
        AudioHoldMode::Timecode
    ));
}
