//! #771 — mv-fps-gate CLI (default features, no probe deps).
//!
//! Reads an OBS log (the newest `%APPDATA%\obs-studio\logs\*.txt` on strih/stream, or the
//! `~/.config/obs-studio/logs/*.txt` on imag), takes each Multiview projector's LATEST
//! `multiview-audit:` sample, and alarms if any projector's measured render cadence fell below
//! its own printed floor (`canvas/2 − tolerance`, #771). This is the E2E-preflight / drift-guard
//! consumer of the audit line the vendored libobs `render_display()` emits — so the user's
//! binding "multiview fps must be measured AND guarded against a drop" requirement has a machine
//! check, not just a log a human might read.
//!
//! Input: a log file path as the first argument, or the log text on stdin if no argument.
//!
//! **Exit codes:**
//! - `0` — every projector's latest sample is at or above its floor → prints `PASS monitor=N ...`
//! - `1` — one or more projectors below floor → prints `FAIL monitor=N rendered_fps=X < floor=F`
//! - `2` — no `multiview-audit:` line found / read error → prints `ERROR: ...` to stderr
//!
//! The RENDER-cadence floor here is distinct from the receive-side `genlock-fifo audit` counters
//! (`genlock-jitter-report`) and from the program render-budget gate (`render-budget-gate`).

use camera_box::mv_audit::{gate_log, GateOutcome};
use std::io::Read;

fn read_input() -> Result<String, String> {
    let mut args = std::env::args().skip(1);
    match args.next() {
        Some(path) => {
            std::fs::read_to_string(&path).map_err(|e| format!("reading log file '{path}': {e}"))
        }
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("reading stdin: {e}"))?;
            Ok(buf)
        }
    }
}

fn main() {
    let log = match read_input() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(2);
        }
    };

    match gate_log(&log) {
        GateOutcome::NoSamples => {
            eprintln!(
                "ERROR: no `multiview-audit:` line in the log — the MV fps audit never emitted \
                 (OBS not running the #771 build, or the log window is shorter than the ~5s audit \
                 period)"
            );
            std::process::exit(2);
        }
        GateOutcome::Clean(samples) => {
            for s in &samples {
                println!(
                    "PASS monitor={} divisor={} rendered_fps={:.1} (floor {:.1}, target {:.0}, {}x{})",
                    s.monitor, s.divisor, s.rendered_fps, s.floor_fps, s.target_fps, s.cx, s.cy
                );
            }
            std::process::exit(0);
        }
        GateOutcome::Breach(breaches) => {
            for s in &breaches {
                println!(
                    "FAIL monitor={} divisor={} rendered_fps={:.1} < floor={:.1} (target {:.0}, {}x{}) \
                     — multiview render collapsed",
                    s.monitor, s.divisor, s.rendered_fps, s.floor_fps, s.target_fps, s.cx, s.cy
                );
            }
            std::process::exit(1);
        }
    }
}
