//! Behavioral guard for `scripts/clock-offset-painter-gate.sh` — the dev1<->painter clock-offset
//! precondition for the all-cambox sweep (#326; a #312 Phase-2 robustness gate).
//!
//! In the ALL_CAMBOX sweep (recording-e2e.sh ALL_CAMBOX=1) each program-switch WINDOW boundary is
//! stamped on dev1's CLOCK_REALTIME, while recording-verdict --switch-schedule partitions the ONE
//! stream recording by the cam2-PAINTER burn gen_ts_ns. Those are two different machines' clocks.
//! The per-cambox attribution holds only if dev1's clock is aligned to the painter's to within the
//! verdict's transition guard (DEFAULT_TRANSITION_GUARD_NS = 1 s). If dev1 drifts past the guard,
//! frames near every boundary are misattributed -> false gaps/copies (false FAIL) or a hidden real
//! loss (false PASS). This gate measures the dev1<->painter offset and FAILS FAST otherwise.
//!
//! The gate REUSES the unit-tested pure comparator in clock-offset-guard.sh (painter_offset_check,
//! tested directly in tests/clock_offset_guard.rs); these tests pin the gate's own FLOW: its
//! end-to-end exit-code contract (PASS within guard / FAIL on drift / INCOMPLETE on an unread
//! offset), the SKIP_CLOCK_OFFSET_ASSERT opt-out, and its usage-error handling. The live reads
//! (local journalctl for dev1, SSH for the painter) are bypassed for these no-rig tests by feeding
//! pre-captured journald text via DEV1_DANTE_JOURNAL / PAINTER_DANTE_JOURNAL (file paths) — the
//! same "caller pre-fetches the status to a file" pattern dantesync-gate.sh uses for Windows nodes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/clock-offset-painter-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Run the gate as a subprocess with extra env; return (exit_code, stdout, stderr).
fn run_gate(args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args).current_dir(manifest_dir());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run clock-offset-painter-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write a one-line DanteSync journald fixture with the given signed µs offset and return its path.
fn write_journal(name: &str, offset_us: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("painter-gate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.log"));
    let mut f = std::fs::File::create(&path).unwrap();
    // The real DanteSync line shape the offset_us_from_journal parser is proven against.
    writeln!(
        f,
        "Jun 15 09:11:53 NODE dantesync[1]: [NTP] offset:{offset_us}us (threshold:520us, adaptive)"
    )
    .unwrap();
    path
}

/// Write a journald fixture that carries NO `[NTP] offset:` line (only a PTP drift line) — so the
/// offset is UNREAD (UNKNOWN), the case the gate must fail closed on.
fn write_journal_no_offset(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("painter-gate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.log"));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "Jun 15 09:11:53 NODE dantesync[1]: [PTP] NANO  Drift: +10ns/s  Adj: +6ppm"
    )
    .unwrap();
    path
}

fn env_pair<'a>(dev1: &'a Path, painter: &'a Path) -> [(&'a str, String); 2] {
    [
        ("DEV1_DANTE_JOURNAL", dev1.display().to_string()),
        ("PAINTER_DANTE_JOURNAL", painter.display().to_string()),
    ]
}

#[test]
fn gate_passes_when_dev1_and_painter_offsets_are_within_the_guard() {
    // dev1 +300 µs, painter +280 µs -> |Δ|=20 µs, far within the default 200 ms guard -> PASS (0).
    let dev1 = write_journal("dev1_ok", "+300");
    let painter = write_journal("painter_ok", "+280");
    let e = env_pair(&dev1, &painter);
    let (code, stdout, _e) = run_gate(
        &["--painter", "cam2=10.77.9.62"],
        &[(e[0].0, e[0].1.as_str()), (e[1].0, e[1].1.as_str())],
    );
    assert_eq!(code, 0, "in-guard offsets must PASS: {stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn gate_fails_fast_when_the_offset_exceeds_the_guard() {
    // dev1 +300 µs, painter -500000 µs -> |Δ|=500300 µs (500 ms) > the 200 ms guard -> FAIL (20).
    // This is the silent-#312-mis-attribution risk turned into a loud pre-sweep abort.
    let dev1 = write_journal("dev1_drift", "+300");
    let painter = write_journal("painter_drift", "-500000");
    let e = env_pair(&dev1, &painter);
    let (code, _o, stderr) = run_gate(
        &["--painter", "cam2=10.77.9.62"],
        &[(e[0].0, e[0].1.as_str()), (e[1].0, e[1].1.as_str())],
    );
    assert_eq!(
        code, 20,
        "out-of-guard offset must FAIL (20). stderr: {stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
}

#[test]
fn custom_guard_us_tightens_the_bound() {
    // dev1 +300 µs, painter 0 µs -> |Δ|=300 µs: PASSES the default 200 ms guard, but a tight
    // --guard-us 100 turns the same 300 µs divergence into a FAIL (20). Proves --guard-us is honored.
    let dev1 = write_journal("dev1_tight", "+300");
    let painter = write_journal("painter_tight", "+0");
    let e = env_pair(&dev1, &painter);
    let (code_default, _o, _e) = run_gate(
        &["--painter", "cam2=10.77.9.62"],
        &[(e[0].0, e[0].1.as_str()), (e[1].0, e[1].1.as_str())],
    );
    assert_eq!(code_default, 0, "300 us is within the default 200 ms guard");
    let (code_tight, _o2, stderr) = run_gate(
        &["--painter", "cam2=10.77.9.62", "--guard-us", "100"],
        &[(e[0].0, e[0].1.as_str()), (e[1].0, e[1].1.as_str())],
    );
    assert_eq!(
        code_tight, 20,
        "300 us > the tightened 100 us guard must FAIL (20). stderr: {stderr}"
    );
}

#[test]
fn gate_incomplete_when_an_offset_is_unread() {
    // The painter journal has no `[NTP] offset:` line -> painter offset UNKNOWN -> the gate cannot
    // certify attribution and must fail closed (11), NEVER a silent pass (test-strictness).
    let dev1 = write_journal("dev1_present", "+300");
    let painter = write_journal_no_offset("painter_noofs");
    let e = env_pair(&dev1, &painter);
    let (code, _o, stderr) = run_gate(
        &["--painter", "cam2=10.77.9.62"],
        &[(e[0].0, e[0].1.as_str()), (e[1].0, e[1].1.as_str())],
    );
    assert_eq!(
        code, 11,
        "an unread offset must be INCOMPLETE (11), not a silent pass. stderr: {stderr}"
    );
    assert!(stderr.contains("GATE INCOMPLETE"), "stderr: {stderr}");
}

#[test]
fn skip_env_bypasses_the_gate_loudly() {
    // SKIP_CLOCK_OFFSET_ASSERT=1 short-circuits to exit 0 — but says SKIPPED loudly so an unguarded
    // run is never mistaken for a verified one. The drifted offsets below would otherwise FAIL.
    let dev1 = write_journal("dev1_skip", "+300");
    let painter = write_journal("painter_skip", "-500000");
    let (code, stdout, _e) = run_gate(
        &["--painter", "cam2=10.77.9.62"],
        &[
            ("DEV1_DANTE_JOURNAL", dev1.display().to_string().as_str()),
            (
                "PAINTER_DANTE_JOURNAL",
                painter.display().to_string().as_str(),
            ),
            ("SKIP_CLOCK_OFFSET_ASSERT", "1"),
        ],
    );
    assert_eq!(code, 0, "SKIP must exit 0: {stdout}");
    assert!(
        stdout.contains("SKIPPED"),
        "SKIP must announce itself loudly: {stdout}"
    );
}

#[test]
fn bad_guard_us_is_a_usage_error() {
    let (code, _o, stderr) = run_gate(&["--guard-us", "abc"], &[]);
    assert_eq!(code, 1, "non-integer --guard-us must be a usage error (1)");
    assert!(stderr.contains("guard-us"), "stderr: {stderr}");
}

#[test]
fn bad_painter_target_is_a_usage_error() {
    // A --painter without NAME=IP must fail loudly, not silently query a bogus host.
    let (code, _o, stderr) = run_gate(&["--painter", "cam2"], &[]);
    assert_eq!(
        code, 1,
        "--painter without NAME=IP must be a usage error (1)"
    );
    assert!(stderr.contains("painter"), "stderr: {stderr}");
}

#[test]
fn help_describes_the_offset_check_and_guard() {
    let (code, stdout, _e) = run_gate(&["--help"], &[]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("offset") && low.contains("guard"),
        "help must describe the dev1<->painter offset check + its guard: {stdout}"
    );
}
