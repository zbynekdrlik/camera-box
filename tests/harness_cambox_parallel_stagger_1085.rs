//! #1085 — ROOT-CAUSE fix for the #715 parallel-restore connection burst: stagger the launch
//! concurrency at recording-e2e.sh's device-restore launch sites, and retire the interim
//! label→IP coupling #715's mitigation introduced.
//!
//! ## The bug (#715, confirmed root cause)
//!
//! cleanup()'s device-restore phase backgrounds cam1 + every active secondary + cam2/painter's
//! ssh restore ALL in the same instant, then one shared `cambox_parallel_wait_and_report`. The N
//! simultaneous ssh CONNECTIONS are connection-rejected within ~1.93 s (a dev1-side burst;
//! #715: 0/18 across 6 CI runs — never a real timeout). #712/#713 made the launches simultaneous
//! to bound a GH-Actions cancellation's stranding window by the slowest box — correct for
//! cancellation, but that same simultaneity concentrates the connection burst.
//!
//! ## The fix these tests lock
//!
//! Each backgrounded restore subshell first calls `cambox_parallel_stagger` (a new pure function
//! in scripts/lib/cambox-parallel-restore.sh) which sleeps `launch_index * CAMBOX_PARALLEL_STAGGER_MS`
//! before its ssh — where `launch_index` = `${#CAMBOX_PARALLEL_PIDS[@]}` read at fork time (the
//! parent appends the PID only AFTER the `&`). So connection ESTABLISHMENT is spread out, while the
//! parent never blocks (all subshells still backgrounded at ~t=0 → #712/#713's cancellation-window
//! benefit is fully preserved — no box is ever "unreached"). The whole phase stays bounded by the
//! slowest box + a tiny (N-1)*gap tail, NOT the sum.
//!
//! Additionally the launch sites now record an EXPLICIT `CAMBOX_PARALLEL_IPS` array; the sequential
//! retry (`cambox_parallel_retry_failed`) PREFERS that explicit IP over `cambox_parallel_label_ip`
//! (which is retained only as a fail-open fallback for a caller that did not populate the IP array).
//!
//! No rig, no ssh: `timeout`/`sshpass` are shadowed by fakes (the #712/#713/#744 argv-logging idiom).

use std::fs;
use std::process::{Command, Stdio};
use std::time::Instant;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/cambox-parallel-restore.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// The body of cleanup() — same slice every sibling `harness_recording_e2e_*` cleanup test uses.
fn cleanup_body(s: &str) -> String {
    let start = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    s[start..end].to_string()
}

/// Extract `NAME() { ... }` — from the literal `"NAME() {"` header to the next top-level `"\n}\n"`.
fn function_body(s: &str, name: &str) -> String {
    let header = format!("{name}() {{");
    let start = s
        .find(&header)
        .unwrap_or_else(|| panic!("expected recording-e2e.sh to define {name}()"));
    let rel_end = s[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("expected {name}() to close with a top-level }}"));
    s[start..start + rel_end + "\n}".len()].to_string()
}

/// The WHOLE device-restore phase — from the shared array init through the single
/// `cambox_parallel_wait_and_report` (identical slicing to harness_cambox_parallel_restore_713.rs).
fn device_restore_phase(body: &str) -> &str {
    let start = body
        .find("CAMBOX_PARALLEL_PIDS=()")
        .expect("cleanup() must initialize CAMBOX_PARALLEL_PIDS before cam1's own restore");
    let wait_pos = body[start..]
        .find("cambox_parallel_wait_and_report")
        .map(|i| start + i)
        .expect("cleanup() must call cambox_parallel_wait_and_report in the device-restore phase");
    let end = body[wait_pos..]
        .find('\n')
        .map(|i| wait_pos + i)
        .unwrap_or(body.len());
    &body[start..end]
}

fn run(script: &str) -> (String, String, bool) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .expect("run driver");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

// ---------------------------------------------------------------------------------------------
// A — PURE LIB: cambox_parallel_stagger sleeps launch_index * gap (the headline RED proof).
// ---------------------------------------------------------------------------------------------

/// With a populated CAMBOX_PARALLEL_PIDS of size k and CAMBOX_PARALLEL_STAGGER_MS=gap, one call to
/// cambox_parallel_stagger must sleep ~k*gap ms — i.e. the delay is proportional to how many
/// restores were already launched (this box's 0-based launch index).
#[test]
fn stagger_sleeps_proportional_to_launch_index() {
    let path = lib_path();
    // index 0 → ~0ms (no wait for the first box); index 3 @ 120ms → ~360ms.
    let script = format!(
        "source '{path}'\n\
         export CAMBOX_PARALLEL_STAGGER_MS=120\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         t0=$(date +%s%3N); cambox_parallel_stagger; t1=$(date +%s%3N); echo \"IDX0=$((t1-t0))\"\n\
         CAMBOX_PARALLEL_PIDS=(1 2 3)\n\
         t0=$(date +%s%3N); cambox_parallel_stagger; t1=$(date +%s%3N); echo \"IDX3=$((t1-t0))\"\n"
    );
    let (stdout, stderr, ok) = run(&script);
    assert!(
        ok,
        "driver must exit 0. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let idx0 = grab_ms(&stdout, "IDX0=");
    let idx3 = grab_ms(&stdout, "IDX3=");
    assert!(
        idx0 < 60,
        "#1085: the FIRST launched box (index 0) must NOT be delayed (got {idx0}ms). stdout:\n{stdout}"
    );
    assert!(
        (300..=600).contains(&idx3),
        "#1085: an index-3 box at 120ms gap must sleep ~360ms (3*120), bounded [300,600] (got \
         {idx3}ms) — the delay must be proportional to the launch index. stdout:\n{stdout}"
    );
}

/// CAMBOX_PARALLEL_STAGGER_MS=0 fully disables the stagger even for a high launch index — the seam
/// the two existing #712/#713 wall-clock parallelism drivers use to isolate ssh-round-trip timing.
#[test]
fn stagger_disabled_when_ms_zero() {
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         export CAMBOX_PARALLEL_STAGGER_MS=0\n\
         CAMBOX_PARALLEL_PIDS=(1 2 3 4 5)\n\
         t0=$(date +%s%3N); cambox_parallel_stagger; t1=$(date +%s%3N); echo \"OFF=$((t1-t0))\"\n"
    );
    let (stdout, stderr, ok) = run(&script);
    assert!(
        ok,
        "driver must exit 0. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let off = grab_ms(&stdout, "OFF=");
    assert!(
        off < 60,
        "#1085: CAMBOX_PARALLEL_STAGGER_MS=0 must disable the stagger entirely (got {off}ms). \
         stdout:\n{stdout}"
    );
}

fn grab_ms(stdout: &str, key: &str) -> u64 {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or_else(|| panic!("expected a {key} line. stdout:\n{stdout}"))
        .trim()
        .parse::<u64>()
        .unwrap_or_else(|e| panic!("parse {key}: {e}. stdout:\n{stdout}"))
}

// ---------------------------------------------------------------------------------------------
// C — WIRING: each launch subshell calls cambox_parallel_stagger.
// ---------------------------------------------------------------------------------------------

#[test]
fn lib_defines_cambox_parallel_stagger() {
    let s = read("scripts/lib/cambox-parallel-restore.sh");
    assert!(
        s.contains("cambox_parallel_stagger()"),
        "#1085: the lib must define cambox_parallel_stagger()"
    );
    assert!(
        s.contains("CAMBOX_PARALLEL_STAGGER_MS"),
        "#1085: the stagger must be configurable via CAMBOX_PARALLEL_STAGGER_MS"
    );
    assert!(
        !s.contains("\nexit ") && !s.contains(" exit "),
        "#1085: the lib must never itself `exit` — cleanup()'s trap must always run to completion."
    );
}

#[test]
fn every_restore_launch_subshell_calls_the_stagger() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let phase = device_restore_phase(&body);
    // Each of the three restore subshells is `( cambox_parallel_stagger; timeout ... root@<IP> ... ) &`
    // — so a `cambox_parallel_stagger` call must appear a short distance BEFORE each ssh target (the
    // first statement inside that subshell). Region-boundary-free: the stagger sits ABOVE each ssh
    // anchor, so slicing "from the anchor" would wrongly exclude it.
    for (name, target) in [
        ("cam1", "root@\"$CAM1_IP\""),
        ("secondary-loop", "root@\"$_cip\""),
        ("cam2/painter", "root@\"$PAINTER_IP\""),
    ] {
        let tpos = phase
            .find(target)
            .unwrap_or_else(|| panic!("#1085: the device-restore phase must contain {target}"));
        let stagger_pos = phase[..tpos]
            .rfind("cambox_parallel_stagger")
            .unwrap_or_else(|| panic!("#1085: no cambox_parallel_stagger before {target}"));
        assert!(
            tpos - stagger_pos < 220,
            "#1085: the {name} restore subshell (ssh {target}) must call cambox_parallel_stagger as \
             its first statement — found the nearest stagger {} bytes before it, too far to be the \
             same subshell. Phase:\n{phase}",
            tpos - stagger_pos
        );
    }
}

// ---------------------------------------------------------------------------------------------
// D — WIRING + BEHAVIOUR: explicit IP array recorded, and the retry PREFERS it over the label.
// ---------------------------------------------------------------------------------------------

#[test]
fn every_launch_site_records_an_explicit_ip() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let phase = device_restore_phase(&body);
    assert!(
        phase.contains("CAMBOX_PARALLEL_IPS=()"),
        "#1085: the device-restore phase must initialize CAMBOX_PARALLEL_IPS. Phase:\n{phase}"
    );
    for needle in [
        "CAMBOX_PARALLEL_IPS+=(\"$CAM1_IP\")",
        "CAMBOX_PARALLEL_IPS+=(\"$_cip\")",
        "CAMBOX_PARALLEL_IPS+=(\"$PAINTER_IP\")",
    ] {
        assert!(
            phase.contains(needle),
            "#1085: each launch site must record its explicit IP — missing {needle:?}. Phase:\n{phase}"
        );
    }
}

/// The retry must use the EXPLICIT CAMBOX_PARALLEL_IPS entry, NOT parse the label. Proof: a label
/// with NO parseable IP but an explicit IP set — the retry must still contact that IP (if it fell
/// back to the label parser it would find nothing and never contact the box).
#[test]
fn retry_prefers_the_explicit_ip_over_the_label() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("calls.log");
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         CAM_PW=fake\n\
         CAMBOX_PARALLEL_RETRY_DELAY=0\n\
         timeout() {{ shift; echo \"CALLED $*\" >> '{log}'; return 0; }}\n\
         sshpass() {{ :; }}\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         CAMBOX_PARALLEL_IPS=()\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam3 no-ip-in-label\"); CAMBOX_PARALLEL_IPS+=(\"10.77.9.63\")\n\
         cambox_parallel_wait_and_report\n\
         echo \"RC=$?\"\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n",
        log = log.to_string_lossy()
    );
    let (stdout, stderr, _ok) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("10.77.9.63"),
        "#1085: the retry must contact the EXPLICIT IP (10.77.9.63) even though the label carries \
         no parseable IP — proving it no longer parses presentation text. calls:\n{calls}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("recovered on sequential retry"),
        "#1085: with the explicit IP the source box must recover on retry. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=0") && stdout.contains("RC=0"),
        "#1085: the recovered box must be pruned and RC drop to 0. stdout:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------------------------
// E — END-TO-END: the REAL device-restore phase staggers connections yet stays concurrent.
// ---------------------------------------------------------------------------------------------

fn write_phase_driver(driver_path: &std::path::Path, log_path: &std::path::Path, stagger_ms: u32) {
    let src = read("scripts/recording-e2e.sh");
    let body = cleanup_body(&src);
    let phase = device_restore_phase(&body).to_string();
    let secondary_ip_fn = function_body(&src, "camera_secondary_ip");

    let camera_set_sh = format!("{}/scripts/camera-set.sh", env!("CARGO_MANIFEST_DIR"));
    let parallel_lib = lib_path();
    let restart_verify_lib = format!(
        "{}/scripts/lib/camera-box-restart-verify.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let dropin_lib = format!(
        "{}/scripts/lib/rig-test-dropin.sh",
        env!("CARGO_MANIFEST_DIR")
    );

    // Fake `timeout` stamps the CONNECTION time (ms epoch) tagged by args, then simulates a ~400ms
    // ssh round-trip. The stamp lands AFTER cambox_parallel_stagger's own sleep, so it captures the
    // staggered connection instant.
    let driver = format!(
        "#!/usr/bin/env bash\n\
         set -uo pipefail\n\
         export CAMBOX_PARALLEL_STAGGER_MS={stagger_ms}\n\
         source {camera_set_sh:?}\n\
         source {parallel_lib:?}\n\
         source {restart_verify_lib:?}\n\
         source {dropin_lib:?}\n\
         CAM1_IP=10.9.9.1\nCAM3_IP=10.9.9.3\nCAM4_IP=10.9.9.4\nCAM5_IP=10.9.9.5\nCAM6_IP=10.9.9.6\nCAM7_IP=10.9.9.7\n\
         PAINTER_IP=10.9.9.2\nCAMERA_NAME=cam1\n\
         CAM_PW=fake\nRUN_ID=1085001\nCLEANUP_SSH_TIMEOUT=5\nALL_CAMBOX=1\n\
         LOG={log:?}\n\
         timeout() {{ shift; echo \"TS $(date +%s%3N) $*\" >> \"$LOG\"; sleep 0.4; return 0; }}\n\
         export -f timeout\n\
         sshpass() {{ :; }}\n\
         export -f sshpass\n\
         {secondary_ip_fn}\n\
         {phase}\n",
        log = log_path.to_string_lossy(),
    );
    fs::write(driver_path, driver).expect("write driver");
}

/// Extract the first ms-epoch stamp for a given IP from the fake timeout's log.
fn connect_ms_for(log: &str, ip: &str) -> Option<u64> {
    log.lines()
        .filter(|l| l.starts_with("TS ") && l.contains(ip))
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(|t| t.parse::<u64>().ok())
        .min()
}

#[test]
fn real_phase_staggers_connections_yet_stays_concurrent() {
    // Default active set today = cam1 + cam2/painter + cam3 (3 boxes; issue 939). Gap 150ms.
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("calls.log");
    let driver_path = dir.path().join("driver.sh");
    write_phase_driver(&driver_path, &log_path, 150);

    let start = Instant::now();
    let out = Command::new("bash")
        .arg(&driver_path)
        .stdin(Stdio::null())
        .output()
        .expect("run driver");
    let elapsed = start.elapsed();
    assert!(
        out.status.success(),
        "driver must exit 0. stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let log = fs::read_to_string(&log_path).unwrap_or_default();
    // All three default-active boxes must be contacted (never skipped for speed).
    let c1 = connect_ms_for(&log, "10.9.9.1").expect("cam1 must be contacted");
    let c2 = connect_ms_for(&log, "10.9.9.2").expect("cam2/painter must be contacted");
    let c3 = connect_ms_for(&log, "10.9.9.3").expect("cam3 must be contacted");

    // STAGGER: the earliest and latest connection instants must be spread by ~2*gap (3 boxes at
    // 150ms → ~300ms). Without the fix (all fire at ~t=0) this spread is ~0.
    let lo = c1.min(c2).min(c3);
    let hi = c1.max(c2).max(c3);
    let spread = hi - lo;
    assert!(
        spread >= 200,
        "#1085: the 3 restore CONNECTIONS must be staggered (spread ~2*150=300ms; got {spread}ms). \
         A spread near 0 means they all fired in the same instant (the #715 burst). Log:\n{log}"
    );

    // CONCURRENCY preserved: the whole phase must finish well under the SEQUENTIAL sum
    // (3 boxes * 400ms round-trip = 1200ms). Staggered-but-concurrent ≈ 2*150 + 400 = 700ms.
    assert!(
        elapsed.as_millis() < 1000,
        "#1085: the phase must stay CONCURRENT (bounded by slowest box + the small stagger tail, \
         NOT the ~1200ms sequential sum). Elapsed: {:?}. Log:\n{log}",
        elapsed
    );
}
