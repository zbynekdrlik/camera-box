//! #11/#373 — the steady-state record window must OUTLAST the verdict's span floor.
//!
//! Run 7020001 (2026-07-02): the harness slept exactly `DURATION` between StartRecord and
//! StopRecord, so after the verdict trims the lead/tail edge frames the analyzed span was
//! 299.9 s against the `--min-secs 300` floor — a floor the run could NEVER satisfy by
//! construction. The recording window must be `DURATION + RECORD_PAD` (pad > 0) so the
//! *analyzed* span can reach the DURATION floor.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn steady_state_record_window_carries_pad_beyond_duration() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("RECORD_PAD"),
        "recording-e2e.sh must sleep DURATION + RECORD_PAD between StartRecord and StopRecord \
         (edge trims make an exactly-DURATION recording ALWAYS miss the --min-secs DURATION floor)"
    );
}
