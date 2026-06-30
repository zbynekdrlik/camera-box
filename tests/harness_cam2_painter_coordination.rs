//! #367 — recording-e2e.sh must coordinate with the permanent `cam2-painter.service`.
//!
//! ## The conflict
//!
//! cam2 now runs a PERMANENT `cam2-painter.service` that paints the dual-QR (+ colour scale)
//! on /dev/fb0 via the KMS page-flip presenter — it holds the DRM master. When the E2E
//! harness launches its OWN `frame-probe --paint-only` painter on the same /dev/fb0, the two
//! fight for the DRM master and the harness painter fails to take it. So the harness MUST
//! stop `cam2-painter.service` before launching its own painter, and restart it on exit.
//!
//! ## What these tests lock (static read of the shell script — no rig, no ssh)
//!
//! Same PURE-string model as tests/harness_recording_e2e_cleanup_resilient.rs: read the real
//! script and assert (1) the harness stops `cam2-painter` BEFORE launching its own painter,
//! (2) cleanup() restarts it on exit, and (3) both calls are best-effort guarded so a box
//! WITHOUT the service installed does not fail the run.

use std::fs;

fn read_script() -> String {
    let path = format!("{}/scripts/recording-e2e.sh", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (the same slice
/// the sibling cleanup tests use).
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

#[test]
fn harness_stops_cam2_painter_before_launching_its_own_painter() {
    let s = read_script();
    let stop = s.find("systemctl stop cam2-painter").expect(
        "#367: the harness must STOP the permanent cam2-painter.service before launching its \
         own /dev/fb0 painter (else the two fight for the DRM master and the harness painter \
         can't take it)",
    );
    // The harness's own painter launch — the nohup'd frame-probe --paint-only on cam2.
    let launch = s
        .find("frame-probe --paint-only")
        .expect("#367: the harness must launch its own frame-probe --paint-only painter");
    assert!(
        stop < launch,
        "#367: `systemctl stop cam2-painter` (byte {stop}) must come BEFORE the harness's own \
         `frame-probe --paint-only` launch (byte {launch}) — otherwise the permanent painter \
         still holds the DRM master when the harness painter starts."
    );
}

#[test]
fn cleanup_restarts_cam2_painter_service() {
    let body = cleanup_body(&read_script());
    assert!(
        body.contains("systemctl start cam2-painter")
            || body.contains("systemctl restart cam2-painter"),
        "#367: cleanup() must (re)start cam2-painter.service on exit so the permanent painter \
         is restored after the harness frees /dev/fb0 — found neither `systemctl start \
         cam2-painter` nor `systemctl restart cam2-painter` in cleanup()."
    );
}

#[test]
fn cam2_painter_stop_and_start_are_best_effort_guarded() {
    let s = read_script();
    // Every cam2-painter systemctl line must be guarded (|| true / 2>/dev/null) so a cam2 box
    // that does NOT have the service installed cannot fail the run or strand the trap.
    for line in s
        .lines()
        .filter(|l| l.contains("systemctl") && l.contains("cam2-painter"))
    {
        assert!(
            line.contains("|| true") || line.contains("2>/dev/null"),
            "#367: cam2-painter systemctl calls must be best-effort guarded (|| true or \
             2>/dev/null) so a box without the service installed doesn't fail the run. \
             Unguarded line: {line:?}"
        );
    }
    // And there must be at least one such line (guards the filter above isn't vacuous).
    assert!(
        s.lines()
            .any(|l| l.contains("systemctl") && l.contains("cam2-painter")),
        "#367: expected at least one cam2-painter systemctl line in the harness"
    );
}
