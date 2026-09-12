//! #1300 -- CG-chain receiver-side verdict: parity harness + sourced-bash guard for
//! `scripts/lib/cg-chain-verify.sh` + `scripts/cg-chain-verify.sh`.
//!
//! The tool is a SELF-CONTAINED bash/awk REPLICA of two Rust sources of truth (so it needs no
//! runtime binary and is Tier-0 testable). This file is the PARITY GATE that pins the replica to
//! the real code -- the optical-head-end-preflight.md pure-Rust-classifier + shell-replica +
//! parity-harness shape:
//!   * `cg_chain_summarize_window` (awk) vs `camera_box::jitter_audit` parse+group+summarize --
//!     SAME raw-log fixture, identical samples / latency / max_abs_head_skew / every gated delta.
//!   * `cg_chain_verdict` (bash) vs `camera_box::resolume_playback::evaluate` -- SAME window,
//!     identical PASS/FAIL across clean, skew, drop, underrun, relock, late-hold, backward-regime,
//!     too-few-samples, and absent cases.
//!
//! Plus sourced-bash unit tests of the pure lib (run_sourced style, like
//! tests/harness_asio_starve_health_1023.rs) and static-anchor assertions of the orchestrator +
//! the rig-health-audit.py report-only wiring.
//!
//! RED before the lib/script exist (sourcing fails / anchors absent); GREEN after.

use camera_box::jitter_audit::{group_by_source, parse_audit_lines, summarize};
use camera_box::resolume_playback::{evaluate, PlaybackBounds, PlaybackWindow};
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/cg-chain-verify.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(body: &str) -> String {
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// A realistic cg OBS window: two sp-*_video sources (the enumerate filter must pick exactly these),
// one with cumulative counters held flat (clean), plus a trailing asrc line per source.
const FIX_CGOBS: &str = "\
14:00:00.001: genlock-fifo audit 'sp-1_video': received=1000 consumed=999 underruns=0 holds=2 overruns=0 backward_steps=0 dropped_due=5 relocks=1 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 (\u{2248}1 frames @ 43.000fps) ts_head_skew_ms=-4 backward_regime_ticks=2
14:00:05.001: genlock-fifo audit 'sp-1_video': received=1215 consumed=1214 underruns=0 holds=4 overruns=0 backward_steps=0 dropped_due=5 relocks=1 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 ts_head_skew_ms=8 backward_regime_ticks=2
14:00:10.001: genlock-fifo audit 'sp-1_video': received=1430 consumed=1429 underruns=0 holds=6 overruns=0 backward_steps=0 dropped_due=5 relocks=1 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 ts_head_skew_ms=-6 backward_regime_ticks=2
14:00:00.050: genlock-fifo audit 'sp-2_video': received=900 consumed=900 underruns=0 holds=1 overruns=0 backward_steps=0 dropped_due=0 relocks=0 late_holds=0 locked=1 depth=2 peak=4 latency_ms=3 ts_head_skew_ms=3 backward_regime_ticks=0
14:00:05.050: genlock-fifo audit 'sp-2_video': received=1115 consumed=1115 underruns=0 holds=3 overruns=0 backward_steps=0 dropped_due=0 relocks=0 late_holds=0 locked=1 depth=2 peak=4 latency_ms=3 ts_head_skew_ms=-9 backward_regime_ticks=0
08:01:08.657: asrc: source 'sp-1_video' estimated=7.62ppm applied=0.00ppm outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=0 (#803/#806/#960)
08:02:08.657: asrc: source 'sp-2_video' estimated=-18.40ppm applied=-18.40ppm outer_bias=0.00ppm cumulative_correction=0.805ms/60s starved_blocks=0 (#803/#806/#960)
";

// A lossy window for 'cg' -- every pathology delta non-zero + skew over bound.
const FIX_LOSSY: &str = "\
14:00:00.001: genlock-fifo audit 'cg': received=1000 consumed=990 underruns=2 holds=2 overruns=0 backward_steps=0 dropped_due=3 relocks=1 late_holds=4 locked=0 depth=3 peak=5 latency_ms=3 ts_head_skew_ms=-40 backward_regime_ticks=7
14:00:05.001: genlock-fifo audit 'cg': received=1100 consumed=1080 underruns=5 holds=4 overruns=0 backward_steps=0 dropped_due=9 relocks=3 late_holds=10 locked=0 depth=3 peak=5 latency_ms=3 ts_head_skew_ms=52 backward_regime_ticks=15
";

/// Shell FIX literal wrapping `text` into `$LOG` via a quoted heredoc (no expansion in body).
fn log_var(text: &str) -> String {
    format!("LOG=$(cat <<'FIX'\n{text}FIX\n)\n")
}

// ---------------------------------------------------------------------------------------------
// lib shape
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "cg_chain_strip_high_bytes",
        "cg_chain_enumerate_sources",
        "cg_chain_summarize_window",
        "cg_chain_verdict",
        "cg_chain_parse_asrc_ppm",
        "cg_chain_asrc_in_band",
        "cg_chain_csv_header",
        "cg_chain_csv_row",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

#[test]
fn enumerate_filters_sp_video_sources_in_first_seen_order() {
    let body = format!(
        "{}printf '%s\\n' \"$LOG\" | cg_chain_enumerate_sources 'sp-.*_video'",
        log_var(FIX_CGOBS)
    );
    assert_eq!(stdout_of(&body), "sp-1_video\nsp-2_video");
}

// ---------------------------------------------------------------------------------------------
// PARITY 1: bash summarize vs camera_box::jitter_audit parse+group+summarize (SAME raw log)
// ---------------------------------------------------------------------------------------------
fn bash_summary(log: &str, source: &str) -> String {
    let body = format!(
        "{}printf '%s\\n' \"$LOG\" | cg_chain_summarize_window '{source}'",
        log_var(log)
    );
    stdout_of(&body)
}

#[test]
fn summary_parity_with_jitter_audit() {
    for (log, source) in [
        (FIX_CGOBS, "sp-1_video"),
        (FIX_CGOBS, "sp-2_video"),
        (FIX_LOSSY, "cg"),
    ] {
        // Rust source of truth.
        let samples = parse_audit_lines(log);
        let groups = group_by_source(&samples);
        let (_, grp) = groups
            .iter()
            .find(|(s, _)| s == source)
            .expect("source present");
        let sum = summarize(grp).expect("summary");

        // bash replica.
        let line = bash_summary(log, source);
        let f: Vec<&str> = line.split('|').collect();
        assert_eq!(f.len(), 9, "bash summary shape for {source}: {line}");
        assert_eq!(
            f[0].parse::<usize>().unwrap(),
            sum.samples,
            "samples {source}"
        );
        assert_eq!(
            f[1].parse::<u32>().unwrap(),
            sum.latency_ms,
            "latency {source}"
        );
        assert_eq!(
            f[2].parse::<i64>().unwrap(),
            sum.max_abs_head_skew_ms,
            "max_skew {source}"
        );
        assert_eq!(
            f[3].parse::<u64>().unwrap(),
            sum.delta_dropped_due,
            "d_dropped {source}"
        );
        assert_eq!(
            f[4].parse::<u64>().unwrap(),
            sum.delta_underruns,
            "d_underruns {source}"
        );
        assert_eq!(
            f[5].parse::<u64>().unwrap(),
            sum.delta_relocks,
            "d_relocks {source}"
        );
        assert_eq!(
            f[6].parse::<u64>().unwrap(),
            sum.delta_late_holds,
            "d_late_holds {source}"
        );
        assert_eq!(
            f[7].parse::<u64>().unwrap(),
            sum.delta_backward_regime_ticks,
            "d_backward_regime {source}"
        );
        // field 9 = last_locked (the LOCK display column) -- no AuditSummary field mirrors it, so
        // cross-check against the LAST raw sample's `locked` flag directly (1=locked, 0=not).
        let last_locked_rust = if grp.last().unwrap().locked { "1" } else { "0" };
        assert_eq!(f[8], last_locked_rust, "last_locked {source}");
    }
}

// ---------------------------------------------------------------------------------------------
// PARITY 2: bash verdict vs camera_box::resolume_playback::evaluate (SAME window)
// ---------------------------------------------------------------------------------------------
fn bash_verdict(summary_line: &str) -> bool {
    // returns true = PASS
    let out = stdout_of(&format!("cg_chain_verdict '{summary_line}' | head -1"));
    out == "PASS"
}

fn window(
    samples: usize,
    max_skew: i64,
    d_drop: u64,
    d_und: u64,
    d_rel: u64,
    d_late: u64,
    d_brt: u64,
) -> PlaybackWindow {
    PlaybackWindow {
        source: "cg".to_string(),
        samples,
        latency_ms: 3,
        max_abs_head_skew_ms: max_skew,
        delta_dropped_due: d_drop,
        delta_underruns: d_und,
        delta_relocks: d_rel,
        delta_late_holds: d_late,
        delta_backward_regime_ticks: d_brt,
    }
}

#[test]
fn verdict_parity_with_resolume_playback_evaluate() {
    // (samples, max_skew, d_drop, d_und, d_rel, d_late, d_brt)
    let cases = [
        (30, 8, 0, 0, 0, 0, 0),  // clean
        (30, 20, 0, 0, 0, 0, 0), // skew exactly at bound
        (30, 25, 0, 0, 0, 0, 0), // skew over bound
        (30, 8, 3, 0, 0, 0, 0),  // drops
        (30, 8, 0, 2, 0, 0, 0),  // underruns
        (30, 8, 0, 0, 1, 0, 0),  // relocks
        (30, 8, 0, 0, 0, 1, 0),  // late holds
        (30, 8, 0, 0, 0, 0, 4),  // backward regime
        (1, 8, 0, 0, 0, 0, 0),   // too few samples
        (30, 50, 2, 0, 1, 0, 0), // multiple faults
    ];
    let bounds = PlaybackBounds::default();
    for (s, sk, dd, du, dr, dl, db) in cases {
        let w = window(s, sk, dd, du, dr, dl, db);
        let rust_pass = evaluate(&w, &bounds).pass;
        // field order: samples|latency|max_skew|d_drop|d_und|d_rel|d_late|d_brt|last_locked
        let line = format!("{s}|3|{sk}|{dd}|{du}|{dr}|{dl}|{db}|1");
        let bash_pass = bash_verdict(&line);
        assert_eq!(
            rust_pass, bash_pass,
            "verdict parity mismatch for {line}: rust={rust_pass} bash={bash_pass}"
        );
    }
}

#[test]
fn absent_source_fails_in_both() {
    // Rust: an absent source has no window -> the orchestrator treats it as FAIL/ABSENT; the bash
    // replica FAILs on an empty summary line (the absent sentinel), matching "not verifiable".
    let out = stdout_of("cg_chain_verdict '' | head -1");
    assert_eq!(out, "FAIL");
    let reason = stdout_of("cg_chain_verdict '' | tail -n +2");
    assert!(
        reason.contains("ABSENT"),
        "expected ABSENT reason, got {reason}"
    );
}

// ---------------------------------------------------------------------------------------------
// asrc residual band (.claude/rules/asrc-residual-floor.md)
// ---------------------------------------------------------------------------------------------
#[test]
fn asrc_floor_band_passes_the_physical_floor_and_fails_the_port_collision_signature() {
    let body = format!(
        "{}printf '%s\\n' \"$LOG\" | cg_chain_parse_asrc_ppm 'sp-1_video'",
        log_var(FIX_CGOBS)
    );
    assert_eq!(
        stdout_of(&body),
        "7.62",
        "newest estimated ppm for sp-1_video"
    );
    // +7.62 is the Dante-GM-vs-UTC physical floor -> in band.
    assert_eq!(stdout_of("cg_chain_asrc_in_band 7.62 10"), "1");
    // -18.40 is the DVS/PTP port-collision signature -> out of band.
    assert_eq!(stdout_of("cg_chain_asrc_in_band -18.40 10"), "0");
    // no asrc line -> UNKNOWN, never a false out-of-band.
    assert_eq!(stdout_of("cg_chain_asrc_in_band '' 10"), "UNKNOWN");
}

// ---------------------------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------------------------
#[test]
fn csv_header_and_row_have_matching_column_counts() {
    let header = stdout_of("cg_chain_csv_header");
    let row = stdout_of("cg_chain_csv_row '2026-09-12T20:00:00Z' strih cg PASS 8 0 0 0 0 0 7.62");
    assert_eq!(
        header.split(',').count(),
        row.split(',').count(),
        "csv header/row column mismatch:\n{header}\n{row}"
    );
    assert!(
        header.starts_with("ts_utc,hop,source,verdict,"),
        "header: {header}"
    );
}

// ---------------------------------------------------------------------------------------------
// Orchestrator static anchors
// ---------------------------------------------------------------------------------------------
fn read(rel: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn orchestrator_exists_sources_the_lib_and_has_the_flags() {
    let s = read("scripts/cg-chain-verify.sh");
    assert!(
        s.contains(". \"$HERE/lib/cg-chain-verify.sh\""),
        "must source the pure lib"
    );
    assert!(s.contains("--soak-hours"), "missing --soak-hours");
    assert!(s.contains("--csv"), "missing --csv");
    assert!(s.contains("--report-only"), "missing --report-only");
    // verdict tool: exits non-zero (3) on FAIL, NOT always-exit-0.
    assert!(s.contains("exit 3"), "must exit non-zero on FAIL");
    // the sleep between soak windows is an injectable seam (no real sleep in tests).
    assert!(
        s.contains("CG_CHAIN_SLEEP_CMD"),
        "soak sleep must be an injectable seam"
    );
}

// ---------------------------------------------------------------------------------------------
// rig-health-audit.py report-only wiring (#787 resolume-rate exemption unchanged)
// ---------------------------------------------------------------------------------------------
#[test]
fn rig_health_audit_has_report_only_cg_chain_row() {
    let s = read("scripts/rig-health-audit.py");
    assert!(
        s.contains("def check_cg_chain"),
        "check_cg_chain not defined"
    );
    assert!(
        s.contains("check_cg_chain()"),
        "check_cg_chain not called in main()"
    );
    // report-only: the row's verdict is NOTE -> never counted as PASS/WARN/FAIL, never changes exit.
    assert!(
        s.contains("CG_CHAIN_REPORT_VERDICT = \"NOTE\""),
        "the cg-chain row must be report-only (NOTE verdict)"
    );
    // #787 resolume-rate exemption is untouched.
    assert!(
        s.contains(r#"CAMERA_SRC_RE = re.compile(r"^NDI\s+cam\d+$", re.I)"#),
        "the #787 resolume-rate exemption (CAMERA_SRC_RE) must be unchanged"
    );
}

// ---------------------------------------------------------------------------------------------
// docs
// ---------------------------------------------------------------------------------------------
#[test]
fn rule_and_skill_recipe_exist() {
    assert!(
        manifest_dir()
            .join(".claude/rules/cg-chain-verify.md")
            .exists(),
        "the cg-chain-verify rule file must exist"
    );
    let skill = read(".claude/skills/e2e/SKILL.md");
    assert!(
        skill.contains("cg-chain-verify.sh"),
        "the e2e skill must document the run recipe"
    );
}
