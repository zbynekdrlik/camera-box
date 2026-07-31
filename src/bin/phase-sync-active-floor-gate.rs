//! #893 — phase-sync-active-floor-gate CLI (default features, no probe deps).
//!
//! `scripts/phase_sync_active_floor_check.py` reads the LIVE `genlock_latency_ms_src` pins over
//! OBS WebSocket for every camera the caller can reach (active or retired) and shells out HERE
//! for the decision — the SAME shape `phase-sync-gate.rs` already established (JSON in over
//! stdin, decision lives ONLY in `camera_box::phase_sync_active_floor`, never re-derived here).
//!
//! Reads a JSON object from stdin:
//!
//! ```json
//! {"active_set": ["cam1", "cam2", "cam3", "cam4"], "pins": {"cam1": 21, "cam5": 3}}
//! ```
//!
//! `pins` may include ANY camera the caller could read (retired ones too) — the module itself
//! filters down to `active_set` before deciding, so a retired camera's stale pin can never
//! satisfy the term.
//!
//! **Exit codes:**
//! - `0` — PASS: at least one active camera sits at the floor. Prints `PASS <camera>`.
//! - `1` — FAIL: every measured active camera is above the floor, OR no active camera's pin was
//!   even present in `pins`. Prints `FAIL: <reason>`.
//! - `2` — bad JSON / missing/wrong-shaped fields — prints `ERROR: <reason>` to stderr.
//!
//! **Usage (Python harness):**
//! ```python
//! subprocess.run([gate_bin], input=json.dumps({"active_set": [...], "pins": {...}}).encode())
//! ```

use camera_box::phase_sync_active_floor::{phase_sync_active_floor_verdict, ActiveFloorVerdict};
use std::collections::BTreeMap;
use std::io::Read;

fn main() {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("ERROR: reading stdin: {e}");
        std::process::exit(2);
    }

    let raw: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: JSON parse: {e}");
            std::process::exit(2);
        }
    };

    let obj = match raw.as_object() {
        Some(o) => o,
        None => {
            eprintln!("ERROR: expected a JSON object {{\"active_set\": [...], \"pins\": {{...}}}}");
            std::process::exit(2);
        }
    };

    let active_set: Vec<String> = match obj.get("active_set").and_then(|v| v.as_array()) {
        Some(arr) => {
            let mut names = Vec::with_capacity(arr.len());
            for v in arr {
                match v.as_str() {
                    Some(s) => names.push(s.to_string()),
                    None => {
                        eprintln!("ERROR: 'active_set' entries must all be strings, got {v}");
                        std::process::exit(2);
                    }
                }
            }
            names
        }
        None => {
            eprintln!("ERROR: missing/non-array 'active_set' field");
            std::process::exit(2);
        }
    };
    if active_set.is_empty() {
        eprintln!("ERROR: 'active_set' is empty — nothing to gate");
        std::process::exit(2);
    }

    let pins_obj = match obj.get("pins").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => {
            eprintln!("ERROR: missing/non-object 'pins' field");
            std::process::exit(2);
        }
    };

    let mut pins: BTreeMap<String, u32> = BTreeMap::new();
    for (name, v) in pins_obj {
        match v.as_u64() {
            Some(ms) if ms <= u32::MAX as u64 => {
                pins.insert(name.clone(), ms as u32);
            }
            _ => {
                eprintln!("ERROR: pin for '{name}' must be a non-negative integer ms, got {v}");
                std::process::exit(2);
            }
        }
    }

    match phase_sync_active_floor_verdict(&pins, &active_set) {
        ActiveFloorVerdict::Pass { floor_camera } => {
            println!("PASS {floor_camera}");
            std::process::exit(0);
        }
        ActiveFloorVerdict::Fail {
            min_active_ms,
            min_active_camera,
            active_pins,
        } => {
            let table = active_pins
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "FAIL: no active camera at the floor -- lowest active pin is {min_active_camera}={min_active_ms}ms ({table})"
            );
            std::process::exit(1);
        }
        ActiveFloorVerdict::NoActiveCamerasMeasured => {
            println!(
                "FAIL: none of the active_set cameras had a pin in 'pins' -- could not measure any of them"
            );
            std::process::exit(1);
        }
    }
}
