//! #109 — zero-loss-restart-gate CLI (default features, no probe deps).
//!
//! The strict zero-loss restart-survival gate `scripts/recording-e2e.sh`'s optional
//! `ZERO_LOSS_RESTART_GATE` step runs. It ingests TWO `recording-verdict --json` reports (a
//! BEFORE-restart and an AFTER-restart full-chain zero-loss verdict, #186) and decides
//! PASS/FAIL/UNKNOWN via `camera_box::zero_loss_restart_survival::classify` — the single
//! source of truth (mirrors `av-restart-sync-gate.rs` / `render-budget-gate.rs` /
//! `obs-watchdog-gate.rs` / `frozen-camera-gate.rs`).
//!
//! **Args:** `<before.json> <after.json>` — the two files are the RAW JSON
//! `recording-verdict --json` writes (top-level `overall_pass` + `full_chain.zero_loss` /
//! `full_chain.real_drops` / `full_chain.burn_unreadable`).
//!
//! **Exit codes:**
//! - `0` — PASS (zero-loss delivery held across the restart, both measurements clean)
//! - `1` — FAIL (at least one measurement was not zero-loss) or UNKNOWN (an
//!   internally-inconsistent measurement — never treated as a pass)
//! - `2` — bad args / missing file / bad JSON / missing required field
//!
//! **Usage (shell harness):**
//! ```text
//! zero-loss-restart-gate before.json after.json
//! ```

use camera_box::zero_loss_restart_survival::{classify, ZeroLossMeasurement};

fn load_measurement(path: &str) -> Result<ZeroLossMeasurement, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {path}: {e}"))?;
    let overall_pass = v
        .get("overall_pass")
        .and_then(|x| x.as_bool())
        .ok_or_else(|| {
            format!(
                "{path}: missing boolean field 'overall_pass' (this JSON is not a \
             recording-verdict --json report)"
            )
        })?;
    let full_chain = v.get("full_chain").ok_or_else(|| {
        format!(
            "{path}: missing object field 'full_chain' (this run had no full-chain burns \
             configured, or this JSON is not a recording-verdict --json report)"
        )
    })?;
    let full_chain_zero_loss = full_chain
        .get("zero_loss")
        .and_then(|x| x.as_bool())
        .ok_or_else(|| format!("{path}: missing boolean field 'full_chain.zero_loss'"))?;
    let real_drops = full_chain
        .get("real_drops")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| format!("{path}: missing numeric field 'full_chain.real_drops'"))?;
    let burn_unreadable = full_chain
        .get("burn_unreadable")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| format!("{path}: missing numeric field 'full_chain.burn_unreadable'"))?;
    Ok(ZeroLossMeasurement {
        overall_pass,
        full_chain_zero_loss,
        real_drops,
        burn_unreadable,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("ERROR: usage: zero-loss-restart-gate <before.json> <after.json>");
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

    let verdict = classify(before, after);
    match verdict.is_pass() {
        true => println!(
            "PASS: zero-loss delivery held across the restart (before overall_pass={}, after overall_pass={})",
            before.overall_pass, after.overall_pass
        ),
        false => println!("{}: {}", verdict.label(), verdict.reasons().join("; ")),
    }

    std::process::exit(if verdict.is_pass() { 0 } else { 1 });
}
