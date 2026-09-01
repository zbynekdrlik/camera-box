//! #771 — mv-fps-gate CLI (default features, no probe deps).
//!
//! Reads an OBS log (the newest `%APPDATA%\obs-studio\logs\*.txt` on strih/stream, or the
//! `~/.config/obs-studio/logs/*.txt` on imag), takes each Multiview projector's MEDIAN
//! `rendered_fps` over its most recent window (`MV_GATE_MEDIAN_WINDOW` samples, #1212), and alarms
//! if any projector's median render cadence fell below its own printed floor
//! (`target − tolerance`, #771/#776). This is the E2E-preflight / drift-guard
//! consumer of the audit line the vendored libobs `render_display()` emits — so the user's
//! binding "multiview fps must be measured AND guarded against a drop" requirement has a machine
//! check, not just a log a human might read.
//!
//! Input: a log file path as the first argument, or the log text on stdin if no argument.
//!
//! **Exit codes:**
//! - `0` — every projector's window-median cadence is at or above its floor → prints `PASS monitor=N median_fps=X ...`
//! - `1` — one or more projectors' window-median below floor → prints `FAIL monitor=N median_fps=X < floor=F ...`
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
        GateOutcome::Clean(gates) => {
            for g in &gates {
                let s = &g.latest;
                println!(
                    "PASS monitor={} divisor={} median_fps={:.1} over {} sample(s) \
                     (floor {:.1}, target {:.0}, {}x{}, latest rendered_fps {:.1})",
                    s.monitor,
                    s.divisor,
                    g.median_fps,
                    g.window_len,
                    s.floor_fps,
                    s.target_fps,
                    s.cx,
                    s.cy,
                    s.rendered_fps
                );
            }
            std::process::exit(0);
        }
        GateOutcome::Breach(breaches) => {
            for g in &breaches {
                let s = &g.latest;
                println!(
                    "FAIL monitor={} divisor={} median_fps={:.1} < floor={:.1} over {} sample(s) \
                     (target {:.0}, {}x{}, latest rendered_fps {:.1}) — multiview render collapsed",
                    s.monitor,
                    s.divisor,
                    g.median_fps,
                    s.floor_fps,
                    g.window_len,
                    s.target_fps,
                    s.cx,
                    s.cy,
                    s.rendered_fps
                );
            }
            std::process::exit(1);
        }
    }
}
