//! #715 — the #713 6-way (now 3-way) parallel cambox restore group concentrates SSH load and
//! trips the pre-existing #675 connection-contention: at the current 3-box fleet the parallel
//! restore group has a **0% success rate** (18/18 box-attempts across 6 recent CI runs reported
//! `WARNING #712 ... failed/timed out`, all within ~1.93s of launch — a connection-level
//! rejection of the simultaneous-ssh burst, NOT a timeout). The identical command run
//! ONE-AT-A-TIME by the later `#684 FINAL` pass succeeds; recovery therefore relies entirely on
//! that late sequential pass, which still left cam2/painter degraded in 2/6 runs.
//!
//! ## The fix these tests lock (pure-lib mitigation; root-cause launch-stagger is #1085)
//!
//! `cambox_parallel_wait_and_report` runs AFTER the ~2s burst window — where one-at-a-time ssh
//! provably succeeds. So it now does a bounded SEQUENTIAL retry of each failed box:
//!   * `cambox_parallel_label_ip` parses EXACTLY ONE IPv4 from the box's label (fail-open: zero
//!     or multiple matches → no retry, the box stays failed for the #684 pass).
//!   * gated on `[ -n "$CAM_PW" ]` — no credential (unit tests) → never touches the rig.
//!   * the recovery STOPS any transient burn unit FIRST (`systemctl stop 'camera-box-burn-*'`, the
//!     #668 stop-before-pkill ordering so a `Restart=on-failure` respawn can't race the pkill into
//!     "Device or resource busy"), the anchored #626 burn-kill, `systemctl restart camera-box`, a
//!     bounded is-active poll; bounded by `CAMBOX_PARALLEL_RETRY_TIMEOUT` (default 15s).
//!   * a SOURCE cam (cam1/cam3) that comes genuinely active is PRUNED (+ "recovered" line). A
//!     PAINTER box is NEVER pruned here — the retry gives it an extra early restart of camera-box
//!     AND cam2-painter, but "is-active" can't tell a painting monitor from a BLACK one (#863), so
//!     its authoritative verdict + the #860 `::error::` stay with the #684/#863 pass.
//!   * the original `WARNING #712` telemetry is NEVER suppressed.
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

/// A non-painter (source) box that failed the parallel group is recovered by ONE sequential retry:
/// the retry contacts the box's IP, STOPS the burn unit first (#668), prints a recovered line,
/// prunes it from FAILED_LABELS, and the original WARNING #712 telemetry is still emitted. RC drops
/// to 0 once nothing remains failed.
#[test]
fn sequential_retry_recovers_a_failed_source_box_and_prunes_it() {
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
        calls.contains("systemctl stop 'camera-box-burn-"),
        "#668: the retry must STOP the burn unit FIRST (before pkill) to avoid a respawn race. \
         calls:\n{calls}"
    );
    assert!(
        stdout.contains("recovered on sequential retry"),
        "a recovered source box must print a recovered-on-retry line. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=0"),
        "a recovered source box must be pruned from CAMBOX_PARALLEL_FAILED_LABELS. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("RC=0"),
        "wait_and_report must return 0 once every failed box recovered. stdout:\n{stdout}"
    );
}

/// A source box whose retry does NOT bring camera-box active stays failed (never pruned), prints no
/// recovered line, and RC stays 1 — the exit-status→recovery mapping (a 🔵 the review asked to lock).
#[test]
fn sequential_retry_failure_keeps_a_source_box_failed() {
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
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam3 (10.77.9.63)\")\n\
         cambox_parallel_wait_and_report\n\
         echo \"RC=$?\"\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n",
        log = log.to_string_lossy()
    );
    let (stdout, _stderr) = run(&script);
    assert!(
        !stdout.contains("recovered on sequential retry"),
        "a failed retry must NOT print a recovered line. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=1"),
        "a source box whose retry failed must STAY failed. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("RC=1"),
        "wait_and_report must return 1 while a box remains failed. stdout:\n{stdout}"
    );
}

/// The painter is NEVER pruned by the retry — even when the retry ssh SUCCEEDS. The retry still
/// restarts cam2-painter (an extra early restart), but "is-active" can't tell a black monitor from
/// a live one (#863), so the painter stays in the failed set and `cambox_parallel_surface_painter_failure`
/// still fires its ::error::. A generic "recovered" line must NOT appear for it.
#[test]
fn painter_is_never_pruned_and_still_surfaces_error_even_on_ssh_success() {
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
         cambox_parallel_wait_and_report || true\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n\
         cambox_parallel_surface_painter_failure\n",
        log = log.to_string_lossy()
    );
    let (stdout, stderr) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("cam2-painter") && calls.contains("10.77.9.62"),
        "the painter retry must restart cam2-painter (an extra restart the #684 pass omits). calls:\n{calls}"
    );
    assert!(
        calls.contains("systemctl stop 'camera-box-burn-"),
        "#668: the painter retry must also STOP the burn unit first. calls:\n{calls}"
    );
    assert!(
        !stdout.contains("recovered on sequential retry"),
        "the painter must NOT get a generic 'recovered' line — it is never pruned. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=1"),
        "the painter must STAY in CAMBOX_PARALLEL_FAILED_LABELS even when the retry ssh succeeded, \
         so a black monitor can never be masked. stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("::error"),
        "the painter must still surface as a GitHub ::error:: annotation (#860). stderr:\n{stderr}"
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

/// Two failed source boxes are BOTH retried, ONE AT A TIME — both are contacted and both pruned on
/// success. Locks that the sequential retry handles more than one box (a ≥2-box test the review
/// asked for) without dropping or muddling either.
#[test]
fn retry_contacts_every_failed_source_box_and_prunes_all_on_success() {
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
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam1 (source, 10.77.9.61)\")\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam3 (10.77.9.63)\")\n\
         cambox_parallel_wait_and_report\n\
         echo \"RC=$?\"\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n",
        log = log.to_string_lossy()
    );
    let (stdout, _stderr) = run(&script);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("10.77.9.61") && calls.contains("10.77.9.63"),
        "BOTH failed boxes must be contacted by the sequential retry. calls:\n{calls}"
    );
    assert!(
        stdout.contains("FAILED_COUNT=0") && stdout.contains("RC=0"),
        "both recovered boxes must be pruned and RC must drop to 0. stdout:\n{stdout}"
    );
}
