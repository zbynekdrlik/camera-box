//! Regression guards for #185 — the Tier-0 local pre-push gate must NOT compile the heavy
//! `probe` feature, so the shared dev1 `target/` stays bounded (it ballooned to 18 GB and filled
//! the disk to 93% because the local cheap-checks ran with `--all-features`, pulling
//! qrcode/rqrr/image/drm/lz4_flex + 5 extra probe `[[bin]]` targets into the single shared cache,
//! which cargo never GCs — rust-lang/cargo#5026).
//!
//! Two invariants lock the fix so it cannot silently regress:
//!
//!   1. EVERY integration test that imports `camera_box::probe::…` is gated with
//!      `#![cfg(feature = "probe")]`. An UNGATED probe test forces the local
//!      `cargo clippy --all-targets` / `cargo test --no-run` to compile the probe feature (to
//!      satisfy the import), which re-balloons `target/`. The probe tests still run on CI, which
//!      builds with `--all-features` (ci.yml).
//!
//!   2. The CLAUDE.md "Local Build Policy" local pre-push gate does NOT prescribe `--all-features`
//!      or `--features probe`. CI compiles the probe feature; locally we run DEFAULT features only.
//!
//! This test reads repo files only (no probe code), so it runs on the DEFAULT feature set and is
//! itself part of the bounded local gate it guards.

use std::fs;
use std::path::Path;

fn manifest_dir() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

fn read(rel: &str) -> String {
    let path = format!("{}/{}", manifest_dir(), rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// True if the file's source references the probe module (`camera_box::probe`).
fn uses_probe_module(src: &str) -> bool {
    src.contains("camera_box::probe")
}

/// True if the file carries the inner `#![cfg(feature = "probe")]` gate (whitespace-tolerant).
fn is_probe_gated(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim().replace(' ', "");
        t == "#![cfg(feature=\"probe\")]"
    })
}

#[test]
fn every_probe_using_integration_test_is_probe_gated() {
    let tests_dir = format!("{}/tests", manifest_dir());
    let mut offenders: Vec<String> = Vec::new();

    for entry in fs::read_dir(&tests_dir).expect("read tests/ dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // Don't inspect this guard itself — it references the literal "camera_box::probe"
        // string in its own logic/strings, not as a real import.
        if path.file_name().and_then(|n| n.to_str()) == Some("local_build_policy_bounds_target.rs")
        {
            continue;
        }
        let src = fs::read_to_string(&path).expect("read test file");
        if uses_probe_module(&src) && !is_probe_gated(&src) {
            offenders.push(
                Path::new(&path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "These integration tests import `camera_box::probe` but are NOT `#![cfg(feature = \"probe\")]`-gated:\n  {}\n\
         An ungated probe test forces the LOCAL Tier-0 cheap-checks (clippy --all-targets / test --no-run) to \
         compile the heavy `probe` feature, re-ballooning the shared dev1 target/ (#185). Add \
         `#![cfg(feature = \"probe\")]` after the file's doc comment. CI (--all-features) still runs them.",
        offenders.join("\n  ")
    );
}

#[test]
fn claude_md_local_gate_does_not_compile_probe() {
    let claude = read("CLAUDE.md");
    // Isolate the "Local Build Policy" section (up to the next top-level heading).
    let section = claude
        .split_once("## Local Build Policy")
        .map(|(_, rest)| rest)
        .expect("CLAUDE.md has a '## Local Build Policy' section");
    let section = section.split("\n## ").next().unwrap_or(section);

    // The fenced local pre-push command block must NOT prescribe --all-features / --features probe.
    // (Prose elsewhere in the section legitimately MENTIONS them as CI-only / banned-locally, so we
    //  scan only the ``` fenced ``` command block.)
    let mut in_fence = false;
    let mut gate_block = String::new();
    for line in section.lines() {
        if line.trim_start().starts_with("```") {
            // First fence opens the local-gate command block; second closes it.
            if !in_fence {
                in_fence = true;
                continue;
            } else {
                break;
            }
        }
        if in_fence {
            // Strip trailing `#` shell comments — a comment like `# NO --all-features` documents
            // the rule and must not count as the command USING the flag.
            let cmd = match line.split_once('#') {
                Some((before, _comment)) => before,
                None => line,
            };
            gate_block.push_str(cmd);
            gate_block.push('\n');
        }
    }
    assert!(
        !gate_block.is_empty(),
        "CLAUDE.md Local Build Policy must have a fenced local pre-push command block"
    );
    assert!(
        !gate_block.contains("--all-features") && !gate_block.contains("--features probe"),
        "The CLAUDE.md local pre-push gate must NOT use --all-features / --features probe \
         (that compiles the heavy probe feature locally and re-balloons target/, #185). \
         The probe feature is compile-checked + built ON CI ONLY. Gate block was:\n{gate_block}"
    );
}
