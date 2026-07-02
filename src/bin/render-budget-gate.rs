//! #405 / EPIC #406 — render-budget-gate CLI (default features, no probe deps).
//!
//! The strict render-fps gate the rig E2E (recording-e2e.sh) runs while burns are ON
//! and the Multiview is open — the exact state that choked strih 60→27fps on
//! 2026-07-02. A python front reads OBS WS `GetStats` deltas per box and pipes them
//! here; the decision lives ONLY in `camera_box::render_budget::classify` so there is a
//! single source of truth (no threshold duplicated in python).
//!
//! Reads a JSON object mapping a box label to its render sample from stdin:
//!
//! ```json
//! {
//!   "strih":  {"active_fps": 60.0, "avg_render_time_ms": 11.3, "render_skipped_frac": 0.0, "target_fps": 60.0},
//!   "stream": {"active_fps": 30.0, "avg_render_time_ms": 1.4,  "render_skipped_frac": 0.0, "target_fps": 30.0}
//! }
//! ```
//!
//! **Exit codes:**
//! - `0` — every box holds its render budget → prints `PASS` per box
//! - `1` — one or more boxes miss the budget → prints `FAIL <box>: <reasons>`
//! - `2` — bad JSON / missing field → prints `ERROR: <reason>` to stderr
//!
//! **Usage (Python harness):**
//! ```python
//! subprocess.run([gate_bin], input=json.dumps(samples).encode(), capture_output=True)
//! ```

use camera_box::render_budget::{classify, RenderSample, RenderVerdict};
use std::io::Read;

fn field(obj: &serde_json::Value, box_label: &str, key: &str) -> Result<f64, String> {
    obj.get(key)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| format!("box '{box_label}' missing numeric field '{key}'"))
}

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
            eprintln!("ERROR: expected a JSON object {{\"box\": {{sample}}, ...}}");
            std::process::exit(2);
        }
    };

    if obj.is_empty() {
        eprintln!("ERROR: no boxes to gate (empty object)");
        std::process::exit(2);
    }

    let mut any_fail = false;
    // Deterministic order (sorted keys) so the output is stable across runs.
    let mut labels: Vec<&String> = obj.keys().collect();
    labels.sort();

    for label in labels {
        let v = &obj[label];
        let sample = match (
            field(v, label, "active_fps"),
            field(v, label, "avg_render_time_ms"),
            field(v, label, "render_skipped_frac"),
            field(v, label, "target_fps"),
        ) {
            (Ok(active_fps), Ok(avg_render_time_ms), Ok(render_skipped_frac), Ok(target_fps)) => (
                RenderSample {
                    active_fps,
                    avg_render_time_ms,
                    render_skipped_frac,
                },
                target_fps,
            ),
            (a, b, c, d) => {
                for r in [a, b, c, d].into_iter().filter_map(Result::err) {
                    eprintln!("ERROR: {r}");
                }
                std::process::exit(2);
            }
        };

        match classify(sample.0, sample.1) {
            RenderVerdict::Pass => println!(
                "PASS {label}: {:.2}fps / {:.2}ms render (target {:.0}fps)",
                sample.0.active_fps, sample.0.avg_render_time_ms, sample.1
            ),
            RenderVerdict::Fail(reasons) => {
                any_fail = true;
                println!("FAIL {label}: {}", reasons.join("; "));
            }
        }
    }

    std::process::exit(if any_fail { 1 } else { 0 });
}
