//! #1303 — an EXECUTABLE C-vs-Rust parity gate for the receiver-side audio-pairing decision.
//!
//! `src/genlock_audio_pairing.rs` is the Tier-0 authority; the `genlock_audio_*` `static inline`
//! helpers in `vendor/obs-studio/libobs/obs-source.c` are the production port the audio-ingest
//! path (`source_output_audio_data`), the `genlock-fifo audit` audio facet, and the LOCK
//! indicator all consume. The two are required to be numerically identical. A static text anchor
//! (`tests/genlock_preload.rs`) proves the C still *says* the right thing, but libobs is compiled
//! only by the genlock workflows, so nothing else executes it.
//!
//! This gate closes that hole: it lifts the contiguous three-helper block VERBATIM out of
//! `obs-source.c`, compiles it standalone with `cc`, runs the C decisions over a spread of
//! vectors, and requires byte-identical results from
//! [`camera_box::genlock_audio_pairing`] on the same inputs. A divergence — a flipped precedence,
//! a wrong truncation, a renamed health code — fails here in seconds instead of surviving to a
//! live rig.
//!
//! Per the project's test-strictness rule this FAILS LOUDLY rather than skipping if the C
//! toolchain is missing — a parity test that silently passes without running is worse than none.

use camera_box::genlock_audio_pairing::{
    decide_audio_health, genlock_audio_delay_ns, pairing_offset_ms, AudioPairingFacets,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SRC: &str = "vendor/obs-studio/libobs/obs-source.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Lift the contiguous `genlock_audio_present_delay_ns` → `genlock_audio_pairing_offset_ms` →
/// `genlock_audio_decide_health` block VERBATIM out of obs-source.c (slice from the first
/// signature through the last function's closing brace).
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
        "#1303: the three genlock_audio_* helpers are no longer contiguous in {SRC} — keep the block together or the lift splices unrelated code."
    );
    let end = src[last_fn..]
        .find("\n}\n")
        .map(|i| last_fn + i + 3)
        .expect("#1303: genlock_audio_decide_health has no closing brace");
    src[start..end].to_string()
}

fn compile(block: &str, main_body: &str, tag: &str) -> PathBuf {
    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\n");
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
                 genlock_audio_* helpers to prove the C and the Rust authority agree numerically; \
                 it must FAIL rather than skip when the toolchain is absent. Install a C compiler \
                 or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1303: the genlock_audio_* helpers lifted from {SRC} do NOT COMPILE standalone under \
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

#[test]
fn c_audio_delay_matches_the_rust_authority_1303() {
    let block = lift_block();
    let latencies: [u32; 10] = [0, 1, 3, 8, 13, 20, 33, 100, 923, 2000];
    let mut body = String::new();
    for &l in &latencies {
        body.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)genlock_audio_present_delay_ns({l}u));\n"
        ));
    }
    let out = run_lines(&compile(&block, &body, "delay"));
    assert_eq!(out.len(), latencies.len());
    for (l, got) in latencies.iter().zip(&out) {
        let c: u64 = got.trim().parse().expect("delay u64");
        assert_eq!(
            c,
            genlock_audio_delay_ns(*l),
            "#1303: genlock_audio_present_delay_ns({l}) C={c} vs Rust={}",
            genlock_audio_delay_ns(*l)
        );
    }
}

#[test]
fn c_pairing_offset_matches_the_rust_authority_1303() {
    let block = lift_block();
    // (applied_audio_delay_ns, video_latency_ms) spread, including audio held longer / shorter /
    // not-at-all than the video, and the deep program-latency case.
    let cases: [(i64, u32); 8] = [
        (3_000_000, 3),
        (10_000_000, 3),
        (3_000_000, 10),
        (0, 33),
        (923_000_000, 923),
        (2_000_000_000, 2000),
        (16_000_000, 16),
        (33_000_000, 16),
    ];
    let mut body = String::new();
    for (ns, lat) in &cases {
        body.push_str(&format!(
            "    printf(\"%lld\\n\", (long long)genlock_audio_pairing_offset_ms({ns}ll, {lat}u));\n"
        ));
    }
    let out = run_lines(&compile(&block, &body, "offset"));
    assert_eq!(out.len(), cases.len());
    for ((ns, lat), got) in cases.iter().zip(&out) {
        let c: i64 = got.trim().parse().expect("offset i64");
        assert_eq!(
            c,
            pairing_offset_ms(*ns, *lat),
            "#1303: genlock_audio_pairing_offset_ms({ns},{lat}) C={c} vs Rust={}",
            pairing_offset_ms(*ns, *lat)
        );
    }
}

#[test]
fn c_audio_health_matches_the_rust_authority_1303() {
    let block = lift_block();
    // Exhaustive sweep of the three booleans crossed with a set of (pairing_offset, frame_interval)
    // pairs spanning below / at / above the one-frame bound in both signs, at 30 and 60 fps.
    let off_intervals: [(i64, i64); 8] = [
        (0, 33),
        (33, 33),
        (34, 33),
        (-33, 33),
        (-50, 33),
        (16, 16),
        (20, 16),
        (999, 33),
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
    let mut body = String::new();
    for (ae, ip, sat, off, iv) in &vectors {
        body.push_str(&format!(
            "    printf(\"%d\\n\", genlock_audio_decide_health({}, {}, {}, {}ll, {}ll));\n",
            *ae as i32, *ip as i32, *sat as i32, off, iv
        ));
    }
    let out = run_lines(&compile(&block, &body, "health"));
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
