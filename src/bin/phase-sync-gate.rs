//! #438 — phase-sync-gate CLI (default features, no probe deps).
//!
//! `scripts/phase_sync_calibrate.py` (#286), the 4-camera MUTUAL phase-sync auto-set
//! controller, shells out HERE for the offset MATH instead of reimplementing it in Python —
//! the decision lives ONLY in `camera_box::phase_sync::compute_phase_sync_offsets` (9 Rust
//! unit tests, `src/phase_sync.rs`), so there is a single source of truth and the two can
//! never silently drift (closes #438, found during the #406 gate-coverage audit). Mirrors
//! `render-budget-gate.rs`'s stdin-JSON-in / stdout-JSON-out shape and
//! `av-restart-sync-gate.rs`'s fail-closed exit-code convention.
//!
//! Reads a JSON object mapping each camera's strih NDI source name (or any caller-chosen
//! identifier) to its measured cam→strih latency in milliseconds, from stdin:
//!
//! ```json
//! {"NDI cam5": 100.0, "NDI cam1": 90.0, "NDI cam3": 80.0}
//! ```
//!
//! Prints a JSON object mapping each same name to its computed per-source genlock-latency
//! offset (ms, already clamped to `[PHASE_SYNC_FLOOR_MS, PHASE_SYNC_CAP_MS]`) to stdout:
//!
//! ```json
//! {"NDI cam5": 55, "NDI cam1": 65, "NDI cam3": 75}
//! ```
//!
//! **Exit codes:**
//! - `0` — offsets computed for every input camera (prints the JSON object above)
//! - `2` — bad JSON / not an object / a latency value is missing, non-numeric, or non-finite
//!   (NaN/inf) — refuses to guess, never silently drops a camera from the output. This is
//!   STRICTER than the pure kernel (which silently skips non-finite entries and returns a
//!   shorter `Vec`): a CLI caller applying settings to a live rig must get a COMPLETE result
//!   or a loud failure, never a partial one that would `KeyError` downstream.
//!
//! An empty input object is NOT an error (matches the kernel: no cameras, nothing to align) —
//! prints `{}` and exits `0`.
//!
//! **Usage (Python harness):**
//! ```python
//! subprocess.run([gate_bin], input=json.dumps(measured).encode(), capture_output=True)
//! ```

use camera_box::phase_sync::compute_phase_sync_offsets;
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
            eprintln!("ERROR: expected a JSON object {{\"source_name\": latency_ms, ...}}");
            std::process::exit(2);
        }
    };

    // Deterministic order (sorted keys) so re-runs of the same input are byte-identical.
    let mut names: Vec<&String> = obj.keys().collect();
    names.sort();

    let mut measured: Vec<(u32, f64)> = Vec::with_capacity(names.len());
    for (idx, name) in names.iter().enumerate() {
        let latency_ms = match obj[name.as_str()].as_f64() {
            Some(v) if v.is_finite() => v,
            Some(v) => {
                eprintln!(
                    "ERROR: source '{name}': latency {v} is not finite (NaN/inf) — refusing to guess"
                );
                std::process::exit(2);
            }
            None => {
                eprintln!("ERROR: source '{name}': latency value is missing or not a number");
                std::process::exit(2);
            }
        };
        measured.push((idx as u32, latency_ms));
    }

    let offsets = compute_phase_sync_offsets(&measured);
    // Every input passed the finite check above, so the kernel is guaranteed to return
    // exactly one offset per input (it only ever drops non-finite entries, and there are
    // none left at this point) — this assert only fires if that kernel invariant breaks.
    assert_eq!(
        offsets.len(),
        names.len(),
        "phase_sync kernel returned {} offsets for {} finite inputs — invariant broken",
        offsets.len(),
        names.len()
    );

    let mut out = serde_json::Map::new();
    for offset in &offsets {
        let name = names[offset.camera_id as usize];
        out.insert(name.clone(), serde_json::Value::from(offset.offset_ms));
    }
    println!("{}", serde_json::Value::Object(out));
    std::process::exit(0);
}
