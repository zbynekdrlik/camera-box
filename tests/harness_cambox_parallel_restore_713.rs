//! #713 — cleanup()'s cam1 (SOURCE) and cam2/painter device restores still ran SEQUENTIALLY,
//! outside #712's own secondary-camera parallel group (#827: cam5/cam6/cam7 retired, grabber
//! cards returned, boxes powered off, but REVERSIBLY via camera_active_secondary_set() —
//! scripts/camera-set.sh) — extend the pattern to the WHOLE device-restore phase (cam1 + every
//! active secondary camera + cam2/painter, one shared wait).
//!
//! ## The bug (live evidence, 2026-07-12, posted on #713)
//!
//! #712 parallelized ONLY the secondary-camera ALL_CAMBOX loop (the part the original #712
//! incident actually described, back when the fleet still had 7 boxes). cam1's SOURCE-camera
//! restore and cam2/painter's restore are SEPARATE
//! sequential blocks in cleanup() — one BEFORE the loop, one AFTER it — both still vulnerable to
//! a GitHub Actions run cancellation (which kills the runner PROCESS directly, not waiting for a
//! bash trap to finish). Live incident: cancelling a run right after #712 shipped still stranded
//! cam2 specifically — its restore sat sequential-after-the-loop and never got to run before the
//! SIGKILL landed.
//!
//! ## The fix these tests lock
//!
//! cam1's restore, the (already-parallel) secondary-camera loop, and cam2/painter's restore now
//! ALL background their ssh call into the SAME `CAMBOX_PARALLEL_PIDS`/`CAMBOX_PARALLEL_LABELS`
//! arrays (initialized ONCE, before cam1), with a SINGLE `cambox_parallel_wait_and_report` call
//! at the end of the whole device-restore phase (after cam2/painter is armed) — never the
//! per-loop wait #712 shipped. Every existing restore command's literal content (#626/#640/#668
//! anchored pkill pattern, the systemd-run unit stop-first ordering, the #675 restart-verify
//! call, the #309 rig-test-dropin clear) is UNCHANGED — only the invocation + wait timing move.

use std::fs;
use std::process::{Command, Stdio};
use std::time::Instant;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (same slice every
/// sibling `harness_recording_e2e_*` cleanup test uses).
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

/// Extract `NAME() { ... }` — from the literal `"NAME() {"` header to the next top-level `"\n}\n"`
/// — a standalone, sourceable snippet. Every helper this file needs is a simple flat function
/// (no nested braces), so this simple scan is exact.
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

/// The WHOLE device-restore phase: from the (now-shared) array init through the single
/// `cambox_parallel_wait_and_report` call that ends it. Anchored on the FIRST
/// `CAMBOX_PARALLEL_PIDS=()` in the cleanup body (must now be BEFORE cam1's own restore, not
/// inside the ALL_CAMBOX loop) through the first `cambox_parallel_wait_and_report` call found
/// after it.
fn device_restore_phase(body: &str) -> &str {
    let start = body
        .find("CAMBOX_PARALLEL_PIDS=()")
        .expect("#713: cleanup() must initialize CAMBOX_PARALLEL_PIDS before cam1's own restore");
    let wait_pos = body[start..]
        .find("cambox_parallel_wait_and_report")
        .map(|i| start + i)
        .expect(
            "#713: cleanup() must call cambox_parallel_wait_and_report in the device-restore phase",
        );
    let end = body[wait_pos..]
        .find('\n')
        .map(|i| wait_pos + i)
        .unwrap_or(body.len());
    &body[start..end]
}

/// HEADLINE: the array init (`CAMBOX_PARALLEL_PIDS=()`) must come BEFORE cam1's own ssh restore
/// call — i.e. cam1 is now part of the SAME shared group the secondary-camera loop already used,
/// not a separate un-backgrounded call ahead of it.
#[test]
fn cambox_parallel_pids_initialized_before_cam1_restore() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    // Anchor on the secondary-camera loop start too, so this test is immune to
    // `root@"$CAM1_IP" \` ALSO appearing earlier in the script for an unrelated purpose (the
    // [0/8] capture-rate-guard preflight ssh calls use the identical continuation-line shape
    // against the SAME $CAM1_IP var, well before cleanup() even starts) — search for cam1's ssh
    // restore call ONLY within the device-restore region (init..loop_start), not from position 0.
    let init_pos = body
        .find("CAMBOX_PARALLEL_PIDS=()")
        .expect("#713: cleanup() must initialize CAMBOX_PARALLEL_PIDS");
    let loop_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312/#827: cleanup() must have the ALL_CAMBOX secondary restore loop");
    assert!(
        init_pos < loop_start,
        "#713: CAMBOX_PARALLEL_PIDS must be initialized before the secondary-camera loop too \
         (unchanged from #712). init_pos={init_pos} loop_start={loop_start}"
    );
    let cam1_region = &body[init_pos..loop_start];
    assert!(
        cam1_region.contains("root@\"$CAM1_IP\" \\\n"),
        "#713: CAMBOX_PARALLEL_PIDS must be initialized BEFORE (and cam1's own ssh restore call \
         must appear between it and) the secondary-camera loop start — cam1 is now part of the \
         shared parallel group, not a separate sequential call ahead of it. Region:\n{cam1_region}"
    );
}

/// cam1's own restore call must be backgrounded ( ... ) & and its PID/label appended to the
/// shared arrays — exactly like the secondary-camera loop's #712 fix, applied to cam1 too.
#[test]
fn cam1_restore_call_is_backgrounded() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let loop_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312/#827: cleanup() must have the ALL_CAMBOX secondary restore loop");
    let cam1_block = &body[..loop_start];
    assert!(
        cam1_block.contains(") &"),
        "#713: cam1's restore call must be backgrounded ( ... ) & — it is now part of the shared \
         parallel restore group. cam1 block:\n{cam1_block}"
    );
    assert!(
        cam1_block.contains("CAMBOX_PARALLEL_PIDS+=(\"$!\")"),
        "#713: cam1's backgrounded restore must collect its own PID into CAMBOX_PARALLEL_PIDS. \
         cam1 block:\n{cam1_block}"
    );
    assert!(
        cam1_block.contains("CAMBOX_PARALLEL_LABELS+=(\"$CAMERA_NAME (source, $CAM1_IP)\")"),
        "#713: cam1's backgrounded restore must record its own label into \
         CAMBOX_PARALLEL_LABELS. cam1 block:\n{cam1_block}"
    );
}

/// cam2/painter's own restore call must ALSO be backgrounded ( ... ) & and its PID/label
/// appended to the SAME shared arrays — the box the live #713 incident actually stranded.
#[test]
fn cam2_painter_restore_call_is_backgrounded() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let painter_region_start = body
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("cleanup() must have the cam2/painter restore block");
    // Bound the region to just after the painter's own restore command (the next
    // `cambox_parallel_wait_and_report` call), so this doesn't accidentally match some LATER,
    // unrelated ") &" elsewhere in cleanup().
    let region_end = body[painter_region_start..]
        .find("cambox_parallel_wait_and_report")
        .map(|i| painter_region_start + i)
        .expect("#713: cleanup() must call cambox_parallel_wait_and_report after cam2/painter");
    let painter_region = &body[painter_region_start..region_end];
    assert!(
        painter_region.contains(") &"),
        "#713: cam2/painter's restore call must be backgrounded ( ... ) & — it is now part of \
         the shared parallel restore group (the live incident's actual stranded box). Painter \
         region:\n{painter_region}"
    );
    assert!(
        painter_region.contains("CAMBOX_PARALLEL_PIDS+=(\"$!\")"),
        "#713: cam2/painter's backgrounded restore must collect its own PID. Painter region:\n{painter_region}"
    );
    assert!(
        painter_region.contains("CAMBOX_PARALLEL_LABELS+=(\"cam2/painter, $PAINTER_IP\")"),
        "#713: cam2/painter's backgrounded restore must record its own label. Painter region:\n{painter_region}"
    );
}

/// The WHOLE device-restore phase must call `cambox_parallel_wait_and_report` EXACTLY ONCE — no
/// separate per-loop wait (the #712 shape) any more; ONE shared wait covers cam1 + the
/// secondary-camera loop + cam2/painter.
#[test]
fn exactly_one_shared_wait_call_covers_the_whole_device_restore_phase() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let phase = device_restore_phase(&body);
    let count = phase.matches("cambox_parallel_wait_and_report").count();
    assert_eq!(
        count, 1,
        "#713: the device-restore phase must call cambox_parallel_wait_and_report EXACTLY ONCE \
         (a single shared wait for cam1 + the secondary-camera loop + cam2/painter), not once per \
         sub-group. Phase:\n{phase}"
    );
    // And that one call must come AFTER cam2/painter's own PID append — i.e. cam2/painter is
    // armed BEFORE the wait, never after it (which would defeat the point).
    let painter_label_pos = phase
        .find("CAMBOX_PARALLEL_LABELS+=(\"cam2/painter, $PAINTER_IP\")")
        .expect("#713: cam2/painter's label append must be inside the device-restore phase");
    let wait_pos = phase
        .find("cambox_parallel_wait_and_report")
        .expect("checked above");
    assert!(
        painter_label_pos < wait_pos,
        "#713: cam2/painter must be armed BEFORE the shared wait call. Phase:\n{phase}"
    );
}

/// Every existing #626/#640/#668/#675/#309 anchor must survive being folded into the shared
/// group unchanged — belt-and-suspenders local check (the sibling #668/#675/#712 test files
/// already assert these independently; this makes this file self-contained evidence).
#[test]
fn device_restore_phase_preserves_existing_restore_command_content() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let phase = device_restore_phase(&body);
    for needle in [
        "systemctl stop camera-box-burn-${RUN_ID}",
        "pkill -9 -f 'camera-box-burn-[a-z0-9]'",
        "systemctl restart camera-box 2>/dev/null; true",
        "camera_box_verify_active_cmds",
        "for _ccn in $(camera_active_secondary_set); do",
        "_cip=\"$(camera_secondary_ip \"$_ccn\")\"",
        "root@\"$PAINTER_IP\" \"pkill -x frame-probe",
        "rig_test_dropin_clear_cmds",
        "systemctl start cam2-painter",
    ] {
        assert!(
            phase.contains(needle),
            "#713: folding cam1/cam2 into the shared parallel group must NOT change any \
             existing restore command content — missing {needle:?} in phase:\n{phase}"
        );
    }
}

/// Build a runnable driver: source camera-set.sh (real camera_active_secondary_set fact) + the
/// real extracted camera_secondary_ip function + the real extracted device-restore phase, with
/// `timeout`/`sshpass` faked as a fast function that just records it was called and sleeps a
/// short, fixed delay to simulate one ssh round-trip. `active_set_override` lets a test drive a
/// DIFFERENT CAMERA_ACTIVE_SET (e.g. reactivating a retired camera) with zero code changes.
fn write_driver(
    driver_path: &std::path::Path,
    log_path: &std::path::Path,
    active_set_override: Option<&str>,
) {
    let recording_e2e_src = read("scripts/recording-e2e.sh");
    let body = cleanup_body(&recording_e2e_src);
    let phase = device_restore_phase(&body).to_string();
    let secondary_ip_fn = function_body(&recording_e2e_src, "camera_secondary_ip");

    let camera_set_sh = format!("{}/scripts/camera-set.sh", env!("CARGO_MANIFEST_DIR"));
    let parallel_lib = format!(
        "{}/scripts/lib/cambox-parallel-restore.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let restart_verify_lib = format!(
        "{}/scripts/lib/camera-box-restart-verify.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let dropin_lib = format!(
        "{}/scripts/lib/rig-test-dropin.sh",
        env!("CARGO_MANIFEST_DIR")
    );

    let active_set_export = match active_set_override {
        Some(v) => format!("export CAMERA_ACTIVE_SET={v:?}\n"),
        None => String::new(),
    };

    // #1085: this driver isolates ssh-ROUND-TRIP concurrency (the parallelism this file asserts).
    // The launch-site stagger (#1085) is a SEPARATE behaviour proven in
    // tests/harness_cambox_parallel_stagger_1085.rs; disable it here so its added delay never
    // muddies this file's round-trip wall-clock bound (it would otherwise add (N-1)*gap ms).
    let driver = format!(
        "#!/usr/bin/env bash\n\
         set -uo pipefail\n\
         export CAMBOX_PARALLEL_STAGGER_MS=0\n\
         {active_set_export}\
         source {camera_set_sh:?}\n\
         source {parallel_lib:?}\n\
         source {restart_verify_lib:?}\n\
         source {dropin_lib:?}\n\
         CAM1_IP=10.9.9.1\nCAM3_IP=10.9.9.3\nCAM4_IP=10.9.9.4\nCAM5_IP=10.9.9.5\nCAM6_IP=10.9.9.6\nCAM7_IP=10.9.9.7\n\
         PAINTER_IP=10.9.9.2\nCAMERA_NAME=cam1\n\
         CAM_PW=fake\nRUN_ID=713001\nCLEANUP_SSH_TIMEOUT=5\nALL_CAMBOX=1\n\
         LOG={log:?}\n\
         timeout() {{ shift; echo \"CALLED $*\" >> \"$LOG\"; sleep 0.3; return 0; }}\n\
         export -f timeout\n\
         sshpass() {{ :; }}\n\
         export -f sshpass\n\
         {secondary_ip_fn}\n\
         {phase}\n",
        log = log_path.to_string_lossy(),
    );
    fs::write(driver_path, driver).expect("write driver");
}

/// THE HEADLINE REAL PROOF: extract the ACTUAL whole device-restore phase from
/// recording-e2e.sh (no rig, no ssh) and measure wall-clock with ALL_CAMBOX=1 + an EXPLICIT
/// CAMERA_ACTIVE_SET="cam1 cam2 cam3" override (cam1 was retired from the DEFAULT active set
/// 2026-08-22, issue 1110, grabber hardware-defective — the override keeps this test's 3-box
/// parallelism power per .claude/rules/camera-active-set.md's "a default-active-set-proves-
/// parallelism test can lose its power when the default shrinks"), so cam1 + cam2/painter +
/// cam3 = 3 boxes total are contacted (cam4 stays excluded — issue 947, grabber wedges the
/// capture leg). 3 boxes at a simulated ~300ms round-trip each: SEQUENTIAL would take at
/// least 900ms; PARALLEL must take well under that (bounded here at 600ms) — and all 3 boxes must
/// still have genuinely been contacted (never skipped for speed).
#[test]
fn whole_device_restore_phase_runs_in_parallel_not_sequentially() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("calls.log");
    let driver_path = dir.path().join("driver.sh");
    write_driver(&driver_path, &log_path, Some("cam1 cam2 cam3"));

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
    for ip in ["10.9.9.1", "10.9.9.2", "10.9.9.3"] {
        assert!(
            log.contains(ip),
            "#713/#827/issue 939/issue 1110: with an EXPLICIT CAMERA_ACTIVE_SET=\"cam1 cam2 \
             cam3\" override (cam1 retired from the DEFAULT active set 2026-08-22, issue 1110 \
             grabber hw-defect — the override keeps this test's 3-box parallelism power per \
             .claude/rules/camera-active-set.md), all 3 boxes (cam1 + cam2/painter + cam3) must \
             actually be contacted (found in the fake timeout()'s call log) — parallelizing \
             must never skip a box for speed. Log:\n{log}"
        );
    }
    assert!(
        !log.contains("10.9.9.4"),
        "cam4 (issue 947, grabber wedges the capture leg) is not in the CAMERA_ACTIVE_SET=\"cam1 \
         cam2 cam3\" override this test drives and must NOT be contacted by the restore phase. \
         Log:\n{log}"
    );

    assert!(
        elapsed.as_millis() < 600,
        "#713/#898/issue 947: the 3 override-active boxes at a simulated ~300ms round-trip each \
         MUST run in PARALLEL (bounded well under the time a SEQUENTIAL phase would take), not \
         one after another. Elapsed: {:?}",
        elapsed
    );
}

/// #827 REVERSIBILITY PROOF: overriding CAMERA_ACTIVE_SET to bring a RETIRED camera (cam5) back
/// makes the WHOLE device-restore phase actually contact it too — with zero code changes beyond
/// the env var.
#[test]
fn whole_device_restore_phase_contacts_a_reactivated_retired_camera() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("calls.log");
    let driver_path = dir.path().join("driver.sh");
    write_driver(&driver_path, &log_path, Some("cam1 cam2 cam3 cam4 cam5"));

    let out = Command::new("bash")
        .arg(&driver_path)
        .stdin(Stdio::null())
        .output()
        .expect("run driver");
    assert!(
        out.status.success(),
        "driver must exit 0. stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let log = fs::read_to_string(&log_path).unwrap_or_default();
    for ip in ["10.9.9.1", "10.9.9.2", "10.9.9.3", "10.9.9.4", "10.9.9.5"] {
        assert!(
            log.contains(ip),
            "#827: reactivating cam5 via CAMERA_ACTIVE_SET must make the whole device-restore \
             phase contact it too (10.9.9.5) alongside cam1/cam2/cam3/cam4 -- proving the \
             reversal actually works end-to-end. Log:\n{log}"
        );
    }
    assert!(
        !log.contains("10.9.9.6") && !log.contains("10.9.9.7"),
        "#827: cam6/cam7 must stay untouched -- only cam5 was added back. Log:\n{log}"
    );
}
