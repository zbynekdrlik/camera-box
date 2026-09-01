//! #1260 — EXECUTABLE parity gate for the within-tick burn-cache state machine.
//!
//! `vendor/distroav/src/burn-tick-cache.hpp`'s `burn_tick_cache_*` helpers (the shipped bytes the
//! DistroAV QR burn filter uses to prep/stamp ONCE per video tick and reuse on the later
//! within-tick draws) must be byte-for-byte behaviourally identical to the Tier-0 authority
//! [`camera_box::burn_tick_cache::BurnTickCache`]. This COMPILES the header (never a retyped copy)
//! and drives it through the SAME event sequences as the Rust authority, asserting identical
//! prepare/reuse decisions — so a flipped `return` or a missing `= true` diverges here in seconds
//! instead of silently changing the recorded burn frame_id cadence on the rig.
//!
//! `cc` is required (present on every runner + the self-hosted dev boxes). Per test-strictness this
//! FAILS LOUDLY rather than skipping if the toolchain is missing — a gate that silently passes
//! without running is worse than none.
//!
//! Runs under `cargo test` (CI) AND standalone via the #1026 recipe:
//!   `CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 \
//!      --extern camera_box=<rlib> tests/burn_tick_cache_parity.rs` — but the simpler local proof
//! is the direct `cc` selftest in the scratch harness (the header + a truth-table `main`); this
//! committed form is the CI mirror lock.

use camera_box::burn_tick_cache::BurnTickCache;
use std::path::PathBuf;
use std::process::Command;

fn burn_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/distroav/src")
}

fn workdir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "burn_tick_cache_1260_{tag}_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&d).expect("create temp workdir");
    d
}

fn compile(src: &PathBuf, bin: &PathBuf) {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-O1",
            "-I",
        ])
        .arg(burn_dir())
        .arg(src)
        .arg("-o")
        .arg(bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1260: could not run the C compiler `{cc}` ({e}). This gate compiles the shipped \
                 vendored burn-tick-cache.hpp; the vendored tree is otherwise built only by the \
                 genlock workflows, so this test is the only pre-CI check of it."
            )
        });
    assert!(
        out.status.success(),
        "#1260: vendor/distroav/src/burn-tick-cache.hpp failed to compile with {cc} under \
         -Wall -Wextra -Wconversion -Wformat=2:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
}

// A tiny driver: argv is a sequence of events — "T" = on_tick, "R" = on_render (prints 1 if it
// PREPARED, 0 if it REUSED), "A" = abort_prepare. One output char per "R", newline-terminated.
const HARNESS: &str = r#"
#include "burn-tick-cache.hpp"
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv)
{
    struct burn_tick_cache c = {0};
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "T") == 0)      burn_tick_cache_on_tick(&c);
        else if (strcmp(argv[i], "A") == 0) burn_tick_cache_abort_prepare(&c);
        else if (strcmp(argv[i], "R") == 0) printf("%d", burn_tick_cache_on_render(&c) ? 1 : 0);
    }
    printf("\n");
    return 0;
}
"#;

/// Run the SAME event sequence through the Rust authority; return the prepare(1)/reuse(0) string.
fn rust_decisions(events: &[&str]) -> String {
    let mut c = BurnTickCache::new();
    let mut out = String::new();
    for &e in events {
        match e {
            "T" => c.on_tick(),
            "A" => c.abort_prepare(),
            "R" => out.push(if c.on_render() { '1' } else { '0' }),
            other => panic!("unknown event {other}"),
        }
    }
    out
}

#[test]
fn c_burn_tick_cache_matches_rust_authority_1260() {
    let header = burn_dir().join("burn-tick-cache.hpp");
    assert!(
        header.exists(),
        "#1260: missing {} — the within-tick burn-cache mirror is gone.",
        header.display()
    );

    let work = workdir("parity");
    let src = work.join("harness.c");
    let bin = work.join("harness");
    std::fs::write(&src, HARNESS).expect("write harness");
    compile(&src, &bin);

    // Event sequences covering: fresh prep+reuse, a new tick re-prep, once-per-tick with VARYING
    // draw counts, abort_prepare re-arming, and back-to-back ticks with a single draw.
    let sequences: Vec<Vec<&str>> = vec![
        vec!["R", "R", "R"],                          // fresh: 1,0,0
        vec!["R", "R", "T", "R", "R"],                // re-prep after tick: 1,0,1,0
        vec!["T", "R", "T", "R", "R", "R", "T", "R"], // once per tick regardless of draws
        vec!["R", "A", "R", "R"],                     // abort re-arms: 1,1,0
        vec!["T", "R", "T", "R", "T", "R"],           // 1 draw per tick: 1,1,1
        vec!["R", "T", "T", "R"],                     // empty tick in between: 1,1
        vec!["A", "R"],                               // abort on a fresh cache: still 1
    ];

    for seq in &sequences {
        let want = rust_decisions(seq);
        let out = Command::new(&bin)
            .args(seq)
            .output()
            .expect("run C harness");
        assert!(
            out.status.success(),
            "C harness exited non-zero for {seq:?}"
        );
        let got = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        assert_eq!(
            got, want,
            "#1260: C and Rust burn-tick-cache diverged on sequence {seq:?} \
             (C={got:?} Rust={want:?}) — the shipped header no longer matches the authority."
        );
    }
}

#[test]
fn rust_authority_stamps_once_per_tick() {
    // A direct guard on the authority's core invariant (independent of the C toolchain): frame_id
    // advances once per tick, never per draw. Mirrors the module's own unit test at the crate level.
    let mut c = BurnTickCache::new();
    let draws_per_tick = [1usize, 3, 2, 7, 1, 3];
    let mut stamps = 0usize;
    for &draws in &draws_per_tick {
        c.on_tick();
        for _ in 0..draws {
            if c.on_render() {
                stamps += 1;
            }
        }
    }
    assert_eq!(stamps, draws_per_tick.len());
}
