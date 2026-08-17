//! #715 — the #713 6-way (now 3-way) parallel cambox restore group concentrates SSH load and
//! trips the pre-existing #675 connection-contention: at the current 3-box fleet the parallel
//! restore group has a **0% success rate** (18/18 box-attempts across 6 recent CI runs reported
//! `WARNING #712 ... failed/timed out`, all within ~1.93s of launch — a connection-level
//! rejection of the simultaneous-ssh burst, NOT a timeout). The identical command run
//! ONE-AT-A-TIME by the later `#684 FINAL` pass succeeds; recovery therefore relies entirely on
//! that late sequential pass, which still left cam2/painter degraded in 2/6 runs.
//!
//! ## The fix these tests lock (pure-lib mitigation; root-cause launch-stagger is a follow-up)
//!
//! `cambox_parallel_wait_and_report` runs AFTER the ~2s burst window — where one-at-a-time ssh
//! provably succeeds. So it now does a bounded SEQUENTIAL retry of each failed box:
//!   * `cambox_parallel_label_ip` parses EXACTLY ONE IPv4 from the box's label (fail-open: zero
//!     or multiple matches → no retry, the box stays failed for the #684 pass).
//!   * gated on `[ -n "$CAM_PW" ]` — no credential (unit tests) → never touches the rig.
//!   * a generic recovery (anchored burn-kill + `systemctl restart camera-box` + bounded
//!     is-active poll); for a `painter` label it ALSO restarts `cam2-painter.service` and
//!     requires BOTH units active before pruning — a generic restart cannot restore the painter,
//!     and pruning it on a generic "success" would mask a black monitor and defeat the #860
//!     `::error::` surface.
//!   * recovered boxes are pruned from `CAMBOX_PARALLEL_FAILED_LABELS` and get a "recovered on
//!     sequential retry" line; the original `WARNING #712` telemetry is NEVER suppressed.
//!
//! No rig, no ssh: `sshpass`/`ssh`/`timeout` are shadowed by faked functions (the #744/#746
//! argv-logging idiom) so the retry logic gets a genuine RED→GREEN with zero rig contact.

use std::fs;
use std::process::{Command, Stdio};

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/cambox-parallel-restore.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn run(script: &str) -> (String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .expect("run driver");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `cambox_parallel_label_ip` must return EXACTLY ONE IPv4 from each of the three real label
/// shapes, and fail (empty + rc=1) for a label with zero or multiple IPs (fail-open).
#[test]
fn label_ip_extracts_exactly_one_ipv4_or_fails() {
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         o=$(cambox_parallel_label_ip 'cam1 (source, 10.77.9.61)'); echo \"cam1 out=[$o] rc=$?\"\n\
         o=$(cambox_parallel_label_ip 'cam3 (10.77.9.63)'); echo \"cam3 out=[$o] rc=$?\"\n\
         o=$(cambox_parallel_label_ip 'cam2/painter, 10.77.9.62'); echo \"painter out=[$o] rc=$?\"\n\
         o=$(cambox_parallel_label_ip 'cam4 (fail)'); echo \"noip out=[$o] rc=$?\"\n\
         o=$(cambox_parallel_label_ip 'x 10.0.0.1 y 10.0.0.2'); echo \"multi out=[$o] rc=$?\"\n"
    );
    let (stdout, _stderr) = run(&script);
    assert!(
        stdout.contains("cam1 out=[10.77.9.61] rc=0"),
        "cam1 label must yield its single IP. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("cam3 out=[10.77.9.63] rc=0"),
        "cam3 label must yield its single IP. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("painter out=[10.77.9.62] rc=0"),
        "cam2/painter label must yield its single IP. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("noip out=[] rc=1"),
        "a label with no IP must fail-open (empty + rc=1). stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("multi out=[] rc=1"),
        "a label with MULTIPLE IPs must fail-open (empty + rc=1), never guess. stdout:\n{stdout}"
    );
}

/// A non-painter box that failed the parallel group is recovered by ONE sequential retry: the
/// retry contacts the box's IP, prints a recovered line, prunes it from FAILED_LABELS, and the
/// original WARNING #712 telemetry is still emitted. RC drops to 0 once nothing remains failed.
#[test]
fn sequential_retry_recovers_a_failed_non_painter_box_and_prunes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("calls.log");
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         CAM_PW=fake\n\
         CLEANUP_SSH_TIMEOUT=5\n\
         CAMBOX_PARALLEL_RETRY_DELAY=0\n\
         timeout() {{ shift; echo \"CALLED $*\" >> '{log}'; return 0; }}\n\
         sshpass() {{ :; }}\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam1 (source, 10.77.9.61)\")\n\
         cambox_parallel_wait_and_report\n\
         echo \"RC=$?\"\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n",
        log = log.to_string_lossy()
    );
    let (stdout, stderr) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        stderr.contains("WARNING #712") && stderr.contains("cam1 (source, 10.77.9.61)"),
        "the original WARNING #712 telemetry must NOT be suppressed by the retry. stderr:\n{stderr}"
    );
    assert!(
        calls.contains("10.77.9.61"),
        "the sequential retry must actually contact the failed box's IP. calls:\n{calls}"
    );
    assert!(
        stdout.contains("recovered on sequential retry"),
        "a recovered box must print a recovered-on-retry line. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=0"),
        "a recovered box must be pruned from CAMBOX_PARALLEL_FAILED_LABELS. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("RC=0"),
        "wait_and_report must return 0 once every failed box recovered. stdout:\n{stdout}"
    );
}

/// A painter box whose retry does NOT bring BOTH camera-box AND cam2-painter active stays failed
/// (never pruned), prints no recovered line, and `cambox_parallel_surface_painter_failure` still
/// fires its ::error::. The retry command must target cam2-painter.service (a generic camera-box
/// restart cannot restore the painter — the #860 dead-monitor trap).
#[test]
fn painter_retry_targets_cam2_painter_and_stays_failed_when_unverified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("calls.log");
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         CAM_PW=fake\n\
         CAMBOX_PARALLEL_RETRY_DELAY=0\n\
         timeout() {{ shift; echo \"CALLED $*\" >> '{log}'; return 1; }}\n\
         sshpass() {{ :; }}\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam2/painter, 10.77.9.62\")\n\
         cambox_parallel_wait_and_report || true\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n\
         cambox_parallel_surface_painter_failure\n",
        log = log.to_string_lossy()
    );
    let (stdout, stderr) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("cam2-painter") && calls.contains("10.77.9.62"),
        "a painter retry must also restart cam2-painter.service (a generic camera-box restart \
         cannot restore the painter). calls:\n{calls}"
    );
    assert!(
        !stdout.contains("recovered on sequential retry"),
        "an unverified painter retry must NOT print a recovered line. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=1"),
        "an unverified painter must STAY in CAMBOX_PARALLEL_FAILED_LABELS. stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("::error"),
        "an unrecovered painter must still surface as a GitHub ::error:: annotation. stderr:\n{stderr}"
    );
}

/// The retry is gated on CAM_PW: with no credential (the unit-test / no-rig case) NO ssh is ever
/// attempted, even though the label carries a real rig IP — the box simply stays failed for the
/// #684 pass. This is what keeps the whole test suite (and any future test) off the real rig.
#[test]
fn no_retry_without_cam_pw_never_sshes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("calls.log");
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         unset CAM_PW\n\
         CAMBOX_PARALLEL_RETRY_DELAY=0\n\
         timeout() {{ shift; echo \"CALLED $*\" >> '{log}'; return 0; }}\n\
         sshpass() {{ :; }}\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam1 (source, 10.77.9.61)\")\n\
         cambox_parallel_wait_and_report || true\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n",
        log = log.to_string_lossy()
    );
    let (stdout, _stderr) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.trim().is_empty(),
        "without CAM_PW the retry must NEVER ssh (no CALLED lines). calls:\n{calls}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=1"),
        "without CAM_PW the failed box must stay failed for the #684 pass. stdout:\n{stdout}"
    );
}

/// A painter box whose retry brings BOTH units active IS pruned + reported recovered, and no
/// ::error:: is surfaced (the happy painter path — proving the downgrade rule works both ways).
#[test]
fn painter_retry_prunes_when_both_units_verified() {
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
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam2/painter, 10.77.9.62\")\n\
         cambox_parallel_wait_and_report\n\
         echo \"RC=$?\"\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n\
         cambox_parallel_surface_painter_failure\n",
        log = log.to_string_lossy()
    );
    let (stdout, stderr) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("cam2-painter"),
        "the painter retry command must include cam2-painter. calls:\n{calls}"
    );
    assert!(
        stdout.contains("recovered on sequential retry") && stdout.contains("FAILED_COUNT=0"),
        "a verified painter recovery must print recovered + be pruned. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("RC=0"),
        "wait_and_report must return 0 once the painter recovered. stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("::error"),
        "a recovered painter must NOT surface a ::error:: annotation. stderr:\n{stderr}"
    );
}
