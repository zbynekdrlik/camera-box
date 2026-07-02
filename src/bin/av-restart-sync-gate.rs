//! #137 — av-restart-sync-gate CLI (default features, no probe deps).
//!
//! The strict OBS-restart A/V-sync-survival gate `scripts/recording-e2e.sh`'s optional
//! `AV_RESTART_GATE` step runs. It ingests TWO `recording-verdict --av-sync` JSON
//! reports (a BEFORE-restart and an AFTER-restart measurement of the cam2 QPSK
//! marker vs its dual-QR video tick, #188) and decides PASS/FAIL/UNKNOWN via
//! `camera_box::av_restart_sync::classify` — the single source of truth (mirrors
//! `render-budget-gate.rs` / `obs-watchdog-gate.rs` / `frozen-camera-gate.rs`).
//!
//! **Args:** `<before.json> <after.json> [tolerance_ms]` — the two files are the RAW
//! JSON `recording-verdict --av-sync` prints (top-level `av_offset_ms` / `matched` /
//! `mad_ms`). `tolerance_ms` defaults to `av_restart_sync::DEFAULT_TOLERANCE_MS`.
//!
//! **Exit codes:**
//! - `0` — PASS (A/V offset held across the restart within tolerance)
//! - `1` — FAIL (offset drifted beyond tolerance) or UNKNOWN (an untrustworthy
//!   measurement — never treated as a pass)
//! - `2` — bad args / missing file / bad JSON / missing required field
//!
//! **Usage (shell harness):**
//! ```text
//! av-restart-sync-gate before.json after.json 50
//! ```

use camera_box::av_restart_sync::{classify, AvSyncMeasurement, DEFAULT_TOLERANCE_MS};

fn load_measurement(path: &str) -> Result<AvSyncMeasurement, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {path}: {e}"))?;
    let offset_ms = v
        .get("av_offset_ms")
        .and_then(|x| x.as_f64())
        .ok_or_else(|| format!("{path}: missing numeric field 'av_offset_ms'"))?;
    let matched = v
        .get("matched")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| format!("{path}: missing numeric field 'matched'"))?
        as usize;
    let mad_ms = v
        .get("mad_ms")
        .and_then(|x| x.as_f64())
        .ok_or_else(|| format!("{path}: missing numeric field 'mad_ms'"))?;
    Ok(AvSyncMeasurement {
        offset_ms,
        matched,
        mad_ms,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!(
            "ERROR: usage: av-restart-sync-gate <before.json> <after.json> [tolerance_ms]"
        );
        std::process::exit(2);
    }

    let before = match load_measurement(&args[0]) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ERROR: before measurement: {e}");
            std::process::exit(2);
        }
    };
    let after = match load_measurement(&args[1]) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ERROR: after measurement: {e}");
            std::process::exit(2);
        }
    };
    let tolerance_ms = match args.get(2) {
        Some(s) => match s.parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("ERROR: tolerance_ms {s:?} is not a number");
                std::process::exit(2);
            }
        },
        None => DEFAULT_TOLERANCE_MS,
    };

    let verdict = classify(before, after, tolerance_ms);
    match verdict.is_pass() {
        true => println!(
            "PASS: A/V offset held across the OBS restart (before {:.1}ms, after {:.1}ms, tolerance {:.1}ms)",
            before.offset_ms, after.offset_ms, tolerance_ms
        ),
        false => println!("{}: {}", verdict.label(), verdict.reasons().join("; ")),
    }

    std::process::exit(if verdict.is_pass() { 0 } else { 1 });
}
