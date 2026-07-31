//! #712 — recording-e2e.sh's ALL_CAMBOX cleanup restore loop (the ACTIVE secondary cameras,
//! camera_active_secondary_set() — #827: cam5/cam6/cam7 retired but REVERSIBLY, see
//! scripts/camera-set.sh) can be cut off mid-loop by a GitHub Actions run CANCELLATION,
//! stranding cam boxes + leaving burns ON.
//!
//! ## The bug (live incident, 2026-07-12)
//!
//! `gh run cancel` on an in-progress ALL_CAMBOX run left the rig PARTIALLY cleaned:
//! cam3/4/5/6's `camera-box-burn-*` binaries were still running (holding their `/dev/videoN`
//! capture devices), those 4 boxes' `camera-box.service` was crash-looping ("Device or resource
//! busy"), and burns were left ON on every strih NDI input (a visible-QR risk on a live
//! broadcast) — cam1 and cam2 (restored OUTSIDE this loop, earlier/later) were already clean.
//!
//! The existing per-step `timeout` guards (`CLEANUP_SSH_TIMEOUT`/`OBS_CLEANUP_TIMEOUT`) protect
//! against a HUNG step, but a GitHub Actions cancellation kills the runner PROCESS directly — it
//! does not wait for a bash trap's SEQUENTIAL per-cambox loop (one ssh round-trip per box) to
//! finish all iterations before the hard kill lands.
//!
//! ## The fix these tests lock
//!
//! The secondary-camera restore loop now launches every active box's ssh restore call
//! CONCURRENTLY (each wrapped in `( ... ) &`), collects their PIDs, and waits for every one of
//! them via the shared `cambox_parallel_wait_and_report` helper
//! (scripts/lib/cambox-parallel-restore.sh) — so the WHOLE loop's wall-clock time is bounded by
//! the SLOWEST single box, not the SUM of all of them, fitting inside a short cancellation grace
//! window regardless of how many camboxes are active.
//!
//! #827 (2026-07-27, binding owner directive): the loop now iterates
//! `camera_active_secondary_set()` (scripts/camera-set.sh) BY NAME instead of a fixed
//! `"$CAM3_IP" "$CAM4_IP" ...` list — cam5/cam6/cam7 are retired from the DEFAULT active set
//! (grabber cards returned to their owner, boxes powered off) but remain fully resolvable
//! FACTS, so reactivating one is a one-line `CAMERA_ACTIVE_SET` edit, never a code change here.
//! Every ssh command's literal content (the anchored pkill pattern, the systemd stop/restart
//! ordering, the #675 verify call) is UNCHANGED — only the invocation is backgrounded and the
//! membership is now derived — so this is additive to (never a replacement for) the sibling
//! `harness_recording_e2e_*` cleanup tests.

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
/// — a standalone, sourceable snippet. Every helper this file needs (camera_secondary_ip) is a
/// simple flat function (no nested braces), so this simple scan is exact.
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

/// The ALL_CAMBOX secondary-camera restore loop region — same slicing the sibling #668/#675
/// tests use (from the loop's own start to the cam2/painter block's start).
fn loop_region(body: &str) -> &str {
    let loop_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312/#827: cleanup() must have the ALL_CAMBOX secondary restore loop");
    let painter_region_start = body
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("cleanup() must have the cam2/painter restore block");
    &body[loop_start..painter_region_start]
}

#[test]
fn recording_e2e_sources_the_parallel_restore_helper() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("cambox-parallel-restore.sh"),
        "#712: recording-e2e.sh must source scripts/lib/cambox-parallel-restore.sh"
    );
    assert!(
        s.contains(". \"$HERE/lib/cambox-parallel-restore.sh\""),
        "#712: recording-e2e.sh must actually `source` (not just mention) cambox-parallel-restore.sh"
    );
}

/// HEADLINE: the secondary-camera restore loop must background EACH box's ssh restore call
/// (never run them sequentially) and collect its PID for a later wait.
#[test]
fn all_cambox_loop_backgrounds_each_restore_call() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let region = loop_region(&body);
    assert!(
        region.contains(") &"),
        "#712: the secondary-camera restore loop must background each ssh restore call ( ... ) & \
         instead of running them sequentially. Loop region:\n{region}"
    );
    assert!(
        region.contains("CAMBOX_PARALLEL_PIDS+="),
        "#712: the loop must collect each backgrounded call's PID into CAMBOX_PARALLEL_PIDS. \
         Loop region:\n{region}"
    );
    assert!(
        region.contains("CAMBOX_PARALLEL_LABELS+="),
        "#712: the loop must record each box's own label (for per-box pass/fail reporting) into \
         CAMBOX_PARALLEL_LABELS. Loop region:\n{region}"
    );
}

/// The loop must be followed by a WAIT for every collected PID (via the shared helper) — never
/// before all boxes have actually been launched, and never skipped entirely (which would let
/// the trap return before the backgrounded ssh calls finish, defeating the whole point: they'd
/// get killed alongside the rest of the process on a real SIGKILL).
///
/// #713 UPDATE (stale-assertion fix, justified): this test used to require
/// `cambox_parallel_wait_and_report` to be called INSIDE `loop_region` itself, right after the
/// loop's own `done`. #713 intentionally moved that call OUT of the loop — cam1's restore (before
/// the loop) and cam2/painter's restore (after the loop) now join the SAME shared
/// CAMBOX_PARALLEL_PIDS/LABELS group, so ONE combined `cambox_parallel_wait_and_report` call
/// covers all of them (see `tests/harness_cambox_parallel_restore_713.rs`,
/// `exactly_one_shared_wait_call_covers_the_whole_device_restore_phase`, which locks that shape
/// precisely). The loop itself is UNCHANGED — it still only APPENDS to the arrays, never waits —
/// so this test now asserts that narrower, still-true invariant: the loop's `done` is followed,
/// somewhere later in cleanup(), by a wait call — never interleaved into the per-box iteration,
/// and never omitted.
#[test]
fn all_cambox_loop_is_followed_by_a_wait_after_the_loop_body() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let region = loop_region(&body);
    assert!(
        !region.contains("cambox_parallel_wait_and_report"),
        "#713: the wait call must NOT be interleaved inside the secondary-camera loop region any \
         more — it moved to a single shared call after cam2/painter is also armed. Loop region:\n{region}"
    );
    let done_pos = body
        .find("\n    done\n")
        .expect("#712: the secondary-camera loop must still close with `done` (unchanged shape)");
    let wait_pos = body[done_pos..]
        .find("cambox_parallel_wait_and_report")
        .map(|i| done_pos + i)
        .expect(
            "#712/#713: cleanup() must call cambox_parallel_wait_and_report to wait for every \
             backgrounded restore (cam1 + the loop + cam2/painter) before cleanup() can return",
        );
    assert!(
        wait_pos > done_pos,
        "#712/#713: cambox_parallel_wait_and_report must be called AFTER the loop's `done` (once \
         ALL boxes' PIDs are collected), not interleaved into the per-box iteration."
    );
}

/// Every EXISTING #328/#626/#640/#668/#675 anchor inside the loop must survive backgrounding
/// unchanged — this is a belt-and-suspenders local check; the sibling test files already assert
/// these independently, but a local check here makes this file self-contained evidence that
/// backgrounding didn't quietly rewrite the safety-critical command text.
#[test]
fn all_cambox_loop_preserves_existing_restore_command_content() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let region = loop_region(&body);
    for needle in [
        "for _ccn in $(camera_active_secondary_set); do",
        "_cip=\"$(camera_secondary_ip \"$_ccn\")\"",
        "systemctl stop camera-box-burn-",
        "pkill -9 -f 'camera-box-burn-[a-z0-9]'",
        "systemctl restart camera-box 2>/dev/null; true",
        "camera_box_verify_active_cmds",
    ] {
        assert!(
            region.contains(needle),
            "#712: backgrounding the loop must NOT change its existing command content — \
             missing {needle:?} in loop region:\n{region}"
        );
    }
}

/// The shared helper itself: waits for EVERY pid individually (never a bare `wait` that swallows
/// which pid belongs to which box) and reports each box's outcome BY NAME.
#[test]
fn cambox_parallel_wait_and_report_reports_each_box_by_name() {
    let s = read("scripts/lib/cambox-parallel-restore.sh");
    assert!(
        s.contains("cambox_parallel_wait_and_report()"),
        "#712: the helper must define cambox_parallel_wait_and_report()"
    );
    assert!(
        s.contains("CAMBOX_PARALLEL_PIDS"),
        "#712: the helper must consume the CAMBOX_PARALLEL_PIDS array"
    );
    assert!(
        s.contains("CAMBOX_PARALLEL_LABELS"),
        "#712: the helper must consume the CAMBOX_PARALLEL_LABELS array to report by name"
    );
    assert!(
        s.contains("WARNING #712"),
        "#712: the helper must print a WARNING referencing #712 for a failed/timed-out box"
    );
    assert!(
        !s.contains("\nexit ") && !s.contains(" exit "),
        "#712: the helper must never itself `exit` — cleanup()'s trap must always run to \
         completion even when one box's restore failed (mirrors the #649/#675 warn-only \
         discipline)."
    );
}

/// REAL behavioral proof (no rig, no ssh): invoke the actual helper with 3 backgrounded jobs —
/// two that succeed, one that fails — and confirm each is reported under its OWN label, not
/// muddled together.
#[test]
fn cambox_parallel_wait_and_report_attributes_pass_and_fail_correctly() {
    let path = format!(
        "{}/scripts/lib/cambox-parallel-restore.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!(
        "source '{path}'\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 0) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam3 (ok)\")\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam4 (fail)\")\n\
         (exit 0) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam5 (ok)\")\n\
         cambox_parallel_wait_and_report\n\
         echo \"RC=$?\"\n"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .expect("run driver");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stdout.contains("cam3 (ok) restore ok"),
        "cam3's success must be reported by its own label. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("cam5 (ok) restore ok"),
        "cam5's success must be reported by its own label. stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("cam4 (fail)") && stderr.contains("WARNING #712"),
        "cam4's failure must be reported by its own label with a #712 warning. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("cam3 (ok)") && !stderr.contains("cam5 (ok)"),
        "a passing box must never ALSO show up in the failure warning. stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("RC=1"),
        "the helper must return non-zero when ANY box failed. stdout:\n{stdout}"
    );
}

/// Extract the secondary-camera loop's OWN text (the `for ... done` block only, no surrounding
/// `if`) as a standalone, sourceable snippet.
fn loop_snippet(s: &str) -> String {
    let start = s
        .find("for _ccn in $(camera_active_secondary_set); do")
        .expect("#712/#827: recording-e2e.sh must have the secondary-camera restore loop");
    let rel_end = s[start..]
        .find("\n    done\n")
        .expect("#712: the loop must close with `done`");
    s[start..start + rel_end + "\n    done".len()].to_string()
}

/// Build a runnable driver: source camera-set.sh (real camera_active_secondary_set /
/// camera_secondary_ip facts — NOT re-derived here) + the real extracted loop snippet, with
/// `timeout`/`sshpass` faked as a fast function that just records it was called and sleeps a
/// short, fixed delay to simulate one ssh round-trip. `active_set_override` lets a test drive a
/// DIFFERENT CAMERA_ACTIVE_SET (e.g. reactivating a retired camera) with zero code changes —
/// the actual point of the #827 design.
fn write_driver(
    driver_path: &std::path::Path,
    log_path: &std::path::Path,
    active_set_override: Option<&str>,
) {
    let recording_e2e_src = read("scripts/recording-e2e.sh");
    let snippet = loop_snippet(&recording_e2e_src);
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

    let active_set_export = match active_set_override {
        Some(v) => format!("export CAMERA_ACTIVE_SET={v:?}\n"),
        None => String::new(),
    };

    let driver = format!(
        "#!/usr/bin/env bash\n\
         set -uo pipefail\n\
         {active_set_export}\
         source {camera_set_sh:?}\n\
         source {parallel_lib:?}\n\
         source {restart_verify_lib:?}\n\
         CAM3_IP=10.9.9.3\nCAM4_IP=10.9.9.4\nCAM5_IP=10.9.9.5\nCAM6_IP=10.9.9.6\nCAM7_IP=10.9.9.7\n\
         CAM_PW=fake\nRUN_ID=712001\nCLEANUP_SSH_TIMEOUT=5\n\
         LOG={log:?}\n\
         timeout() {{ shift; echo \"CALLED $*\" >> \"$LOG\"; sleep 0.3; return 0; }}\n\
         export -f timeout\n\
         sshpass() {{ :; }}\n\
         export -f sshpass\n\
         {secondary_ip_fn}\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         {snippet}\n\
         cambox_parallel_wait_and_report\n",
        log = log_path.to_string_lossy(),
    );
    fs::write(driver_path, driver).expect("write driver");
}

/// THE HEADLINE REAL PROOF: run the ACTUAL extracted loop from recording-e2e.sh (no rig, no ssh).
/// #898 (2026-07-31): cam3 retired from the DEFAULT active set (grabber card destroyed), so the
/// default secondary set shrank to cam4 ALONE -- too small to prove genuine multi-box
/// parallelism on its own. This test widens via an explicit CAMERA_ACTIVE_SET override
/// (default cam4 + a reactivated cam5, mirroring the reversibility mechanism proven below) to
/// keep exercising 2 REAL concurrent boxes through the actual derived code path. At a simulated
/// ~300ms round-trip each: SEQUENTIAL would take >=600ms; PARALLEL must take well under that
/// (bounded here at 500ms) — and both boxes must still have genuinely been contacted.
#[test]
fn all_cambox_restore_loop_runs_in_parallel_not_sequentially() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("calls.log");
    let driver_path = dir.path().join("driver.sh");
    write_driver(&driver_path, &log_path, Some("cam1 cam2 cam4 cam5"));

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
    for ip in ["10.9.9.4", "10.9.9.5"] {
        assert!(
            log.contains(ip),
            "#712/#898: both active camboxes (cam4/cam5) must actually be contacted (found in \
             the fake timeout()'s call log) — parallelizing must never skip a box for speed. \
             Log:\n{log}"
        );
    }

    assert!(
        elapsed.as_millis() < 500,
        "#712/#898: the cam4/cam5 boxes at a simulated ~300ms round-trip each MUST run in \
         PARALLEL (bounded well under the ~600ms sequential SUM), not one after another. \
         Elapsed: {:?}",
        elapsed
    );
}

/// #827 REVERSIBILITY PROOF: overriding CAMERA_ACTIVE_SET to bring a RETIRED camera (cam5) back
/// makes the loop actually CONTACT it too — with zero code changes beyond the env var. This is
/// the real end-to-end proof (not just a unit test on the pure helper) that the whole restore
/// path honours the one declared active-set list.
#[test]
fn all_cambox_restore_loop_contacts_a_reactivated_retired_camera() {
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
    for ip in ["10.9.9.3", "10.9.9.4", "10.9.9.5"] {
        assert!(
            log.contains(ip),
            "#827/#898: reactivating cam3 and cam5 via CAMERA_ACTIVE_SET must make the restore \
             loop contact both of them too (10.9.9.3, 10.9.9.5), alongside the still-active \
             cam4 -- proving the reversal actually works end-to-end. Log:\n{log}"
        );
    }
    assert!(
        !log.contains("10.9.9.6") && !log.contains("10.9.9.7"),
        "#827: cam6/cam7 must stay untouched -- only cam5 was added back. Log:\n{log}"
    );
}
