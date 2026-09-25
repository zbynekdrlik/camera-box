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
    audio_hold_mode, audio_hold_ms, audio_needs_live_offset, audio_place_shift_ms,
    audio_place_term_ns, audio_wall_to_mono_ns, decide_audio_health, genlock_audio_delay_ns,
    pairing_offset_ms, video_delay_moved, video_delay_reference_ns, video_delay_round_ms,
    video_delay_sample_ns, video_delay_smooth_ns, video_delay_track, AudioHoldMode,
    AudioPairingFacets, VideoDelayTracker,
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
        "genlock_audio_place_shift_ms(",
        "genlock_audio_video_delay_ref_ns(",
        "genlock_audio_pairing_offset_ms(",
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
    let mut seq: Vec<(u64, u64)> = Vec::new();
    seq.extend(std::iter::repeat_n((100_000_000, IV30), 120));
    seq.extend(std::iter::repeat_n((66_666_667, IV30), 150));
    for i in 0..200u64 {
        seq.push((if i % 10 == 0 { 100_000_000 } else { 66_666_667 }, IV30));
    }
    // a 20-tick excursion one frame deeper arms the settle; it comes back to 70 ms, 3 ms off the
    // applied 67 — under half a frame, so the settle must apply NOTHING.
    seq.extend(std::iter::repeat_n((100_000_000, IV30), 20));
    seq.extend(std::iter::repeat_n((70_000_000, IV30), 100));
    seq.extend(std::iter::repeat_n((133_333_333, IV30), 150));
    seq.extend(std::iter::repeat_n((50_000_000, IV60), 120));
    seq.extend(std::iter::repeat_n((58_400_000, IV60), 120));

    let mut body = String::from(
        "    uint64_t sm = 0; uint32_t ap = 0; uint32_t st = 0;\n    static const unsigned long long S[] = {",
    );
    body.push_str(
        &seq.iter()
            .map(|(s, _)| format!("{s}ull"))
            .collect::<Vec<_>>()
            .join(","),
    );
    body.push_str("};\n    static const unsigned long long I[] = {");
    body.push_str(
        &seq.iter()
            .map(|(_, i)| format!("{i}ull"))
            .collect::<Vec<_>>()
            .join(","),
    );
    body.push_str(&format!(
        "}};\n    for (int k = 0; k < {}; k++) {{\n        genlock_video_delay_track(&sm, &ap, &st, S[k], I[k]);\n        printf(\"%llu %u %u\\n\", (unsigned long long)sm, (unsigned)ap, (unsigned)st);\n    }}\n",
        seq.len()
    ));
    let out = run_c(&body, "tracker");
    let mut t = VideoDelayTracker::default();
    let mut want = Vec::new();
    for (s, iv) in &seq {
        video_delay_track(&mut t, *s, *iv);
        want.push(format!(
            "{} {} {}",
            t.smoothed_ns, t.applied_ms, t.settle_ticks
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
    assert!(
        want.iter().any(|l| l.ends_with(" 67 0")),
        "the sequence never applied 67 ms"
    );
    assert!(
        want.iter().any(|l| l.ends_with(" 133 0")),
        "the sequence never applied 133 ms"
    );
    assert!(
        !want.iter().any(|l| l.ends_with(" 70 0")),
        "the reversed excursion must not apply its sub-half-frame 70 ms"
    );
}

#[test]
fn c_audio_hold_and_placement_match_the_rust_authority_1367() {
    let modes: [(bool, u32, bool, u32); 7] = [
        (false, 3, true, 97),
        (true, 0, true, 97),
        (true, 3, true, 0),
        (true, 3, false, 97),
        (true, 3, true, 97),
        (true, 923, true, 1033),
        (true, 3, false, 0),
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
    let shifts: [(i64, i64); 5] = [
        (100_000_000, 133_000_000),
        (-5, 3_000_000),
        (i64::MIN, i64::MAX),
        (0, 0),
        (123_456_789, -987_654_321),
    ];
    let mut body = String::new();
    for (g, l, w, v) in &modes {
        body.push_str(&format!(
            "    {{ int m = genlock_audio_hold_mode({}, {l}u, {}, {v}u); printf(\"%d %u %s\\n\", m, (unsigned)genlock_audio_hold_ms(m, {l}u, {v}u), genlock_audio_hold_token(m)); }}\n",
            *g as i32, *w as i32
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
    for (a, b) in &shifts {
        let lit = |v: i64| {
            if v == i64::MIN {
                "INT64_MIN".to_string()
            } else {
                format!("{v}ll")
            }
        };
        body.push_str(&format!(
            "    printf(\"%.6f\\n\", genlock_audio_place_shift_ms({}, {}));\n",
            lit(*a),
            lit(*b)
        ));
    }
    for m in 0u8..3 {
        for p in 0u8..3 {
            body.push_str(&format!(
                "    printf(\"%d\\n\", genlock_audio_needs_live_offset({m}, {p}) ? 1 : 0);\n"
            ));
        }
    }
    let out = run_c(&body, "hold_place");
    let mode_of = |c: u8| match c {
        0 => AudioHoldMode::Off,
        1 => AudioHoldMode::Latency,
        _ => AudioHoldMode::Timecode,
    };
    let mut want: Vec<String> = Vec::new();
    for (g, l, w, v) in &modes {
        let m = audio_hold_mode(*g, *l, *w, *v);
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
    for (a, b) in &shifts {
        want.push(format!("{:.6}", audio_place_shift_ms(*a, *b)));
    }
    for m in 0u8..3 {
        for p in 0u8..3 {
            want.push((audio_needs_live_offset(mode_of(m), mode_of(p)) as i32).to_string());
        }
    }
    assert_eq!(
        out, want,
        "issue 1367: a C audio hold/placement helper diverged from the Rust authority"
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
    let out = run_c(&body, "offset");
    let mut want: Vec<String> = Vec::new();
    for (s, l) in &refs {
        want.push(video_delay_reference_ns(*s, *l).to_string());
    }
    for (a, v) in &cases {
        want.push(pairing_offset_ms(*a, *v).to_string());
    }
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
