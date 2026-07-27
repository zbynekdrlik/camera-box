//! #657 item 3 — the self-heal "belt": rig-busy-gate.sh converts a PERMANENT self-deadlock
//! (our own stray leftover recording keeping the gate RIG_BUSY forever) into a self-healing one.
//!
//! ## The bug this complements
//!
//! A cancelled recording-e2e.sh run can leave strih and/or stream recording (streaming OFF) —
//! `_stray_recording_hosts` (scripts/obs_phase2.py, #657) already identifies this EXACT signature
//! as provably OUR OWN leftover (a real broadcast always streams+records together). Before this
//! fix, rig-busy-gate.sh only ever POLLED and eventually gave up (RIG_BUSY, exit 42) — every
//! later gate run hit the SAME leftover and died the SAME way until a human manually SSHed in and
//! StopRecorded it. Live incident (2026-07-10, #657): 4+ gate runs failed RIG_BUSY on our own
//! leftovers in one session, ~45 min of rig time lost.
//!
//! ## The fix these tests lock
//!
//! After `STRAY_HEAL_THRESHOLD` (default 3) CONSECUTIVE polls showing a box in EXACTLY the
//! stray signature, the gate itself calls `obs_phase2.py record --host <ip> --action stop`
//! (StopRecord only — keeps the file, never touches program routing) and logs loudly citing
//! #657. This NEVER fires for a box that is also streaming (a real broadcast) — that safety
//! invariant is enforced by `_stray_recording_hosts` itself (obs_phase2.py, tested separately in
//! tests/python/test_obs_phase2_rig_busy_check.py) and re-verified end-to-end here.
//!
//! These tests execute the REAL `scripts/rig-busy-gate.sh` against a smart fake `obs_phase2.py`
//! stub (a real Python script, not just a canned-response double) that tracks call counts and a
//! "healed" flag on disk — so the gate's SELF-HEAL DECISION and the resulting resolution to
//! RIG_FREE are both proven, not just that some log line was printed.

use std::fs;
use std::path::Path;
use std::process::Command;

fn script_path() -> String {
    format!("{}/scripts/rig-busy-gate.sh", env!("CARGO_MANIFEST_DIR"))
}

/// A smart fake obs_phase2.py: `rig-busy-check` reports OUR OWN stray recording on strih
/// (streaming OFF) until a `healed.flag` file appears — which the `record --host ... --action
/// stop` branch writes, simulating the self-heal actually stopping the recording. This lets the
/// test prove the REAL end-to-end resolution (self-heal -> next poll reports free -> gate exits
/// 0), not just that a command was invoked.
fn write_smart_fake(dir: &Path, healed_flag: &Path, record_calls_log: &Path) -> String {
    let path = dir.join("fake_obs_phase2.py");
    let contents = format!(
        r#"import sys, json

HEALED_FLAG = {healed_flag:?}
RECORD_CALLS_LOG = {record_calls_log:?}

def main():
    cmd = sys.argv[1]
    if cmd == "record":
        host = sys.argv[sys.argv.index("--host") + 1]
        with open(RECORD_CALLS_LOG, "a") as f:
            f.write(host + "\n")
        with open(HEALED_FLAG, "w") as f:
            f.write("healed")
        print("")
        return
    # rig-busy-check
    import os
    if os.path.exists(HEALED_FLAG):
        print(json.dumps({{"busy": False, "reasons": []}}))
        return
    print(json.dumps({{
        "busy": True,
        "reasons": ["strih is recording (GetRecordStatus.outputActive=true)"],
        "diagnostics": [
            {{"host": "strih", "streaming": False, "recording": True, "recordTimecode": "01:00:00.000"}},
            {{"host": "stream", "streaming": False, "recording": False, "recordTimecode": None}},
        ],
        "stray_hosts": ["strih"],
    }}))

main()
"#,
        healed_flag = healed_flag.to_string_lossy(),
        record_calls_log = record_calls_log.to_string_lossy(),
    );
    fs::write(&path, contents).expect("write smart fake obs_phase2.py");
    path.to_string_lossy().to_string()
}

/// A fake obs_phase2.py that ALWAYS reports a REAL broadcast (streaming AND recording together
/// on strih) — never a stray signature, so `stray_hosts` is never present. Any `record --action
/// stop` call would be a safety-invariant violation.
fn write_real_broadcast_fake(dir: &Path) -> String {
    let path = dir.join("fake_obs_phase2_broadcast.py");
    let contents = r#"import sys, json
if sys.argv[1] == "record":
    print("SHOULD NEVER BE CALLED FOR A REAL BROADCAST", file=sys.stderr)
    sys.exit(1)
print(json.dumps({
    "busy": True,
    "reasons": [
        "strih is streaming (GetStreamStatus.outputActive=true)",
        "strih is recording (GetRecordStatus.outputActive=true)",
    ],
    "diagnostics": [
        {"host": "strih", "streaming": True, "recording": True, "recordTimecode": "00:05:00.000"},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ],
}))
"#;
    fs::write(&path, contents).expect("write real-broadcast fake obs_phase2.py");
    path.to_string_lossy().to_string()
}

struct RunResult {
    status: i32,
    stdout: String,
}

fn run_gate(fake_py: &str, iterations: &str, sleep_secs: &str, threshold: &str) -> RunResult {
    // #830: isolate the shared cross-repo rig lease to a per-test tempdir -- never the real
    // /var/tmp/rig-lease (see the identical note in harness_rig_busy_gate.rs / CLAUDE.md's
    // shared-checkout GOTCHA).
    let lease_dir = Path::new(fake_py)
        .parent()
        .expect("fake_py must have a parent dir")
        .join("rig-lease-830");
    let out = Command::new("bash")
        .arg(script_path())
        .env("OBS_PHASE2_PY", fake_py)
        .env("RIG_BUSY_GATE_ITERATIONS", iterations)
        .env("RIG_BUSY_GATE_SLEEP_SECS", sleep_secs)
        .env("STRAY_HEAL_THRESHOLD", threshold)
        .env("STRIH_HOST", "10.77.9.202")
        .env("STREAM_HOST", "10.77.9.204")
        .env("RIG_LEASE_DIR", lease_dir)
        .env_remove("GITHUB_STEP_SUMMARY")
        .output()
        .expect("run rig-busy-gate.sh");
    RunResult {
        status: out.status.code().unwrap_or(-1),
        stdout: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// The end-to-end proof: a stray-only signature on strih for STRAY_HEAL_THRESHOLD consecutive
/// polls must trigger EXACTLY ONE `record --host 10.77.9.202 --action stop` call, and the gate
/// must then resolve to RIG_FREE (exit 0) on a LATER poll — never RIG_BUSY.
#[test]
fn self_heal_stoprecords_stray_box_and_gate_resolves_to_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let healed_flag = dir.path().join("healed.flag");
    let record_calls_log = dir.path().join("record_calls.log");
    let fake = write_smart_fake(dir.path(), &healed_flag, &record_calls_log);

    // threshold=2, budget=10 iterations, sleep=0 (fast test): call 1 -> stray streak=1 (no heal),
    // call 2 -> streak=2 >= threshold -> self-heal fires -> record_calls.log written + healed.flag
    // set -> call 3 -> busy=false -> RIG_FREE.
    let r = run_gate(&fake, "10", "0", "2");

    assert_eq!(
        r.status, 0,
        "#657: the gate must resolve to RIG_FREE once the self-heal stops the stray recording, \
         not die RIG_BUSY: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("OUTCOME=RIG_FREE"),
        "must report RIG_FREE once healed: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("#657"),
        "the self-heal action must be logged citing #657: {}",
        r.stdout
    );

    let calls = fs::read_to_string(&record_calls_log).unwrap_or_default();
    let call_lines: Vec<&str> = calls.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        call_lines,
        vec!["10.77.9.202"],
        "#657: self-heal must StopRecord strih (10.77.9.202) EXACTLY ONCE — got: {calls:?}"
    );
}

/// The self-heal must NEVER fire before the consecutive-poll threshold is reached — a single
/// stray-looking poll (e.g. a transient WS hiccup mid-StartRecord) must not trigger a StopRecord.
#[test]
fn self_heal_does_not_fire_before_threshold_reached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let healed_flag = dir.path().join("healed.flag");
    let record_calls_log = dir.path().join("record_calls.log");
    let fake = write_smart_fake(dir.path(), &healed_flag, &record_calls_log);

    // threshold=5 but budget only 2 iterations — the gate must give up (RIG_BUSY) WITHOUT ever
    // having reached the threshold, so the self-heal must never have fired.
    let r = run_gate(&fake, "2", "0", "5");

    assert_eq!(
        r.status, 42,
        "with a budget shorter than the threshold, the gate must still fail RIG_BUSY (self-heal \
         never reached): {}",
        r.stdout
    );
    assert!(
        !record_calls_log.exists() || fs::read_to_string(&record_calls_log).unwrap().is_empty(),
        "#657: the self-heal must NOT fire before STRAY_HEAL_THRESHOLD consecutive polls"
    );
}

/// The safety invariant: a REAL broadcast (streaming AND recording together) must NEVER be
/// self-healed, no matter how many consecutive busy polls occur. The gate must still eventually
/// fail RIG_BUSY (never silently proceed) and must NEVER invoke `record --action stop`.
#[test]
fn self_heal_never_touches_a_real_broadcast() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_real_broadcast_fake(dir.path());

    let r = run_gate(&fake, "5", "0", "2");

    assert_eq!(
        r.status, 42,
        "#657: a real broadcast must still exhaust the budget and fail RIG_BUSY, never be \
         self-healed away: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("SHOULD NEVER BE CALLED"),
        "#657 SAFETY VIOLATION: the self-heal invoked `record --action stop` against a box that \
         is ALSO streaming (a real broadcast) — this must never happen: {}",
        r.stdout
    );
}
