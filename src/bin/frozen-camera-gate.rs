//! #365 — frozen-camera-gate CLI binary (default features, no probe deps).
//!
//! Reads the per-camera hash timeline as JSON from stdin:
//!
//! ```json
//! {
//!   "NDI cam1": ["sha1hex1", null, "sha1hex3"],
//!   "NDI cam2": ["sha1hexA", "sha1hexA", "sha1hexA", "sha1hexA"]
//! }
//! ```
//!
//! Each value is an ordered list of SHA-1 hex strings (one per sample) or `null`
//! (failed capture). Calls `camera_box::frozen_camera::frozen_cameras` for the
//! verdict.
//!
//! **Exit codes:**
//! - `0` — all cameras are live → prints `PASS`
//! - `1` — one or more cameras are frozen → prints `FROZEN: NDI cam1, NDI cam2`
//! - `2` — bad JSON / parse error → prints `ERROR: <reason>` to stderr
//!
//! **Args:** `--threshold N` (default: `FREEZE_THRESHOLD` = 3).
//!
//! **Usage (Python harness):**
//! ```python
//! result = subprocess.run(
//!     [verdict_bin, "--threshold", "3"],
//!     input=json.dumps(timelines).encode(),
//!     capture_output=True,
//! )
//! ```

use camera_box::frozen_camera::{frozen_cameras, CameraTimeline, FREEZE_THRESHOLD};
use std::collections::HashMap;
use std::io::Read;

fn main() {
    let threshold = parse_threshold_arg().unwrap_or(FREEZE_THRESHOLD);

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
            eprintln!("ERROR: expected a JSON object {{\"camera_name\": [hashes...], ...}}");
            std::process::exit(2);
        }
    };

    let mut timelines: HashMap<String, CameraTimeline> = HashMap::new();
    for (name, arr) in obj {
        let list = match arr.as_array() {
            Some(a) => a,
            None => {
                eprintln!("ERROR: camera '{name}' value is not an array");
                std::process::exit(2);
            }
        };
        let timeline: CameraTimeline = list
            .iter()
            .map(|v| {
                if v.is_null() {
                    None
                } else {
                    v.as_str().map(str::to_owned)
                }
            })
            .collect();
        timelines.insert(name.clone(), timeline);
    }

    let frozen = frozen_cameras(&timelines, threshold);
    if frozen.is_empty() {
        println!("PASS");
        std::process::exit(0);
    } else {
        println!("FROZEN: {}", frozen.join(", "));
        std::process::exit(1);
    }
}

/// Parse `--threshold N` from argv. Returns `None` if the flag is absent or unparseable.
fn parse_threshold_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        if arg == "--threshold" {
            if let Some(val) = it.next() {
                return val.parse().ok();
            }
        }
    }
    None
}
