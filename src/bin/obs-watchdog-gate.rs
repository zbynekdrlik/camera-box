//! #391 — obs-watchdog-gate CLI (default features, no probe deps).
//!
//! The strict broadcast-OBS liveness/wedge gate `scripts/obs-liveness-watchdog.sh` runs
//! on a dev1 systemd timer. A python front (`scripts/obs-liveness-probe.py`) measures
//! `GetStats` over OBS WebSocket for each box (always reachable, no ssh/MCP needed) and
//! OPTIONALLY layers in agent/MCP-only process signals (obs64 count / `Responding` /
//! CPU%) and pipes the combined sample here; the decision lives ONLY in
//! `camera_box::obs_watchdog::classify` so there is a single source of truth (mirrors
//! `render-budget-gate.rs` / `frozen-camera-gate.rs`).
//!
//! Reads a JSON object mapping a box label to its health sample from stdin:
//!
//! ```json
//! {
//!   "strih":  {"ws_reachable": true, "active_fps": 60.0, "avg_render_time_ms": 11.3,
//!              "render_skipped_frac": 0.003, "target_fps": 60.0,
//!              "obs64_count": 1, "responding": true, "cpu_percent": 5.7},
//!   "stream": {"ws_reachable": true, "active_fps": 29.5, "avg_render_time_ms": 9.0,
//!              "render_skipped_frac": 0.16, "target_fps": 30.0,
//!              "obs64_count": 1, "responding": false, "cpu_percent": 168.0}
//! }
//! ```
//!
//! `ws_reachable` and `target_fps` are REQUIRED. Every other field is OPTIONAL — omit or
//! pass JSON `null` for a signal the caller could not sample this pass (e.g. a plain
//! dev1 timer pass with no agent/MCP process-state check has no `obs64_count` /
//! `responding` / `cpu_percent`).
//!
//! **Exit codes:**
//! - `0` — every box HEALTHY
//! - `1` — one or more boxes NOT healthy → prints `<LABEL-VERDICT> <box>: <reasons>`
//! - `2` — bad JSON / missing required field → prints `ERROR: <reason>` to stderr
//!
//! **Usage (Python harness):**
//! ```python
//! subprocess.run([gate_bin], input=json.dumps(samples).encode(), capture_output=True)
//! ```

use camera_box::obs_watchdog::{classify, ObsHealthSample};
use std::io::Read;

fn opt_f64(v: &serde_json::Value, key: &str) -> Result<Option<f64>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("field '{key}' is not a number")),
    }
}

fn opt_u32(v: &serde_json::Value, key: &str) -> Result<Option<u32>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("field '{key}' is not a non-negative integer")),
    }
}

fn opt_bool(v: &serde_json::Value, key: &str) -> Result<Option<bool>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("field '{key}' is not a bool")),
    }
}

fn req_f64(v: &serde_json::Value, box_label: &str, key: &str) -> Result<f64, String> {
    v.get(key)
        .and_then(|x| x.as_f64())
        .ok_or_else(|| format!("box '{box_label}' missing required numeric field '{key}'"))
}

fn req_bool(v: &serde_json::Value, box_label: &str, key: &str) -> Result<bool, String> {
    v.get(key)
        .and_then(|x| x.as_bool())
        .ok_or_else(|| format!("box '{box_label}' missing required bool field '{key}'"))
}

fn sample_from_json(
    box_label: &str,
    v: &serde_json::Value,
) -> Result<(ObsHealthSample, f64), String> {
    let ws_reachable = req_bool(v, box_label, "ws_reachable")?;
    let target_fps = req_f64(v, box_label, "target_fps")?;
    let active_fps = opt_f64(v, "active_fps")?;
    let avg_render_time_ms = opt_f64(v, "avg_render_time_ms")?;
    let render_skipped_frac = opt_f64(v, "render_skipped_frac")?;
    let obs64_count = opt_u32(v, "obs64_count")?;
    let responding = opt_bool(v, "responding")?;
    let cpu_percent = opt_f64(v, "cpu_percent")?;
    Ok((
        ObsHealthSample {
            ws_reachable,
            active_fps,
            avg_render_time_ms,
            render_skipped_frac,
            obs64_count,
            responding,
            cpu_percent,
        },
        target_fps,
    ))
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

    let mut labels: Vec<&String> = obj.keys().collect();
    labels.sort();

    let mut any_unhealthy = false;
    for label in labels {
        let v = &obj[label];
        let (sample, target_fps) = match sample_from_json(label, v) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(2);
            }
        };

        let verdict = classify(sample, target_fps);
        if verdict.is_healthy() {
            println!(
                "HEALTHY {label}: activeFps={:?} avgRenderMs={:?} renderSkip={:?}",
                sample.active_fps, sample.avg_render_time_ms, sample.render_skipped_frac
            );
        } else {
            any_unhealthy = true;
            println!(
                "{} {label}: {}",
                verdict.label(),
                verdict.reasons().join("; ")
            );
        }
    }

    std::process::exit(if any_unhealthy { 1 } else { 0 });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_sample_with_all_process_fields() {
        let v = json!({
            "ws_reachable": true,
            "active_fps": 60.0,
            "avg_render_time_ms": 11.3,
            "render_skipped_frac": 0.003,
            "target_fps": 60.0,
            "obs64_count": 1,
            "responding": true,
            "cpu_percent": 5.7
        });
        let (sample, target_fps) = sample_from_json("strih", &v).expect("should parse");
        assert!(sample.ws_reachable);
        assert_eq!(target_fps, 60.0);
        assert_eq!(sample.active_fps, Some(60.0));
        assert_eq!(sample.obs64_count, Some(1));
        assert_eq!(sample.responding, Some(true));
        assert_eq!(sample.cpu_percent, Some(5.7));
    }

    #[test]
    fn parses_ws_only_sample_with_no_process_fields() {
        // The plain dev1-timer pass: only GetStats, no agent/MCP process signals at all.
        let v = json!({
            "ws_reachable": true,
            "active_fps": 30.0,
            "avg_render_time_ms": 1.4,
            "render_skipped_frac": 0.0,
            "target_fps": 30.0
        });
        let (sample, _) = sample_from_json("stream", &v).expect("should parse");
        assert_eq!(sample.obs64_count, None);
        assert_eq!(sample.responding, None);
        assert_eq!(sample.cpu_percent, None);
    }

    #[test]
    fn explicit_null_process_fields_parse_as_none() {
        let v = json!({
            "ws_reachable": false,
            "target_fps": 60.0,
            "obs64_count": null,
            "responding": null,
            "cpu_percent": null
        });
        let (sample, _) = sample_from_json("strih", &v).expect("should parse");
        assert_eq!(sample.obs64_count, None);
        assert_eq!(sample.responding, None);
        assert_eq!(sample.cpu_percent, None);
    }

    #[test]
    fn missing_ws_reachable_is_a_parse_error() {
        let v = json!({"target_fps": 60.0});
        let err = sample_from_json("strih", &v).unwrap_err();
        assert!(err.contains("ws_reachable"));
    }

    #[test]
    fn missing_target_fps_is_a_parse_error() {
        let v = json!({"ws_reachable": true});
        let err = sample_from_json("strih", &v).unwrap_err();
        assert!(err.contains("target_fps"));
    }

    #[test]
    fn wrong_type_for_optional_field_is_a_parse_error() {
        let v = json!({"ws_reachable": true, "target_fps": 60.0, "obs64_count": "one"});
        let err = sample_from_json("strih", &v).unwrap_err();
        assert!(err.contains("obs64_count"));
    }

    #[test]
    fn negative_obs64_count_is_a_parse_error() {
        // A negative count is nonsensical; the JSON number itself would need to be an
        // i64 to even represent -1, which as_u64() already rejects — locking that here.
        let v = json!({"ws_reachable": true, "target_fps": 60.0, "obs64_count": -1});
        let err = sample_from_json("strih", &v).unwrap_err();
        assert!(err.contains("obs64_count"));
    }
}
