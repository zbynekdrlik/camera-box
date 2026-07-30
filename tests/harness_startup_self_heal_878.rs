//! #878 — a dead `recording-e2e.sh` (SIGKILLed / GH Actions cancelled) strands whatever it took
//! over: `camera-box.service` on every cambox the ALL_CAMBOX sweep stopped, plus the permanent
//! cam2-painter, plus a leaked genlock_burn (#844). `cleanup()` — the bash `EXIT` trap — is the
//! ONLY place that gives rig state back, and it is structurally unreachable on SIGKILL, which
//! `full-path-e2e.yml`'s `cancel-in-progress: true` concurrency group makes routine. The NEXT run
//! then fails at `[0/8]` preflight on a leftover precondition instead of a measurement — four
//! consecutive runs died this way, live, 2026-07-30.
//!
//! ## Same family as #844/#869/#872
//!
//! All four are the same shape: the harness takes rig state hostage and only its own successful
//! cleanup gives it back. The fix here is an IDEMPOTENT STARTUP self-heal — gated STRICTLY on the
//! SAME durable "a harness entered a test state and did not clean up" evidence
//! `rig-restore-watchdog.sh` already trusts (`rig_e2e_marker_present()`, #353) — that repairs
//! `camera-box.service` + the painter + any leaked burn BEFORE the `[0/8]` fleet preflight below
//! asserts anything. It deliberately does NOT change the fleet preflight's own pass/fail policy —
//! that self-heal-vs-hard-fail question for an UNPROVEN inactive box is left open for the user.
//!
//! Two test groups:
//!   (a) `startup_self_heal_decision` / `startup_self_heal_reason` — the PURE decision in
//!       `scripts/lib/startup-self-heal.sh`, sourced directly (mirrors the `run_sourced` /
//!       `preflight_fleet_check_verdict` convention in `tests/harness_preflight_fleet_check_758.rs`).
//!   (b) static-anchor assertions on `scripts/recording-e2e.sh` proving the step is genuinely
//!       WIRED IN — gated on ALL_CAMBOX, running BEFORE the first box is taken over (before the
//!       existing `[0/8]` fleet preflight text), reusing the existing restore primitives
//!       (`camera_box_verify_active_cmds`, `cam2_painter_restore_verify_cmds`,
//!       `obs_burn_filter.py remove`) rather than a parallel mechanism, and deriving its fleet
//!       scope from `camera_active_secondary_set()`/`camera_active_excluding` — never a literal
//!       cam-number range (`.claude/rules/camera-active-set.md`).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/startup-self-heal.sh")
}

fn read_harness() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

struct Run {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Source the pure lib directly and run `body` against it — mirrors
/// `tests/harness_preflight_fleet_check_758.rs`'s `run_sourced` convention. A missing lib file
/// makes `. <path>` fail, so every test in group (a) is genuinely RED before the lib exists.
fn run_sourced(body: &str) -> Run {
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", lib_script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ---------------------------------------------------------------------------------------------
// (a) startup_self_heal_decision / startup_self_heal_reason — pure, no I/O.
// ---------------------------------------------------------------------------------------------

#[test]
fn decision_repairs_when_marker_present() {
    let r = run_sourced("startup_self_heal_decision 1");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(
        r.stdout, "repair",
        "marker present is POSITIVE evidence this harness owns the leftover state -- must repair"
    );
}

#[test]
fn decision_skips_when_marker_absent() {
    let r = run_sourced("startup_self_heal_decision 0");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(
        r.stdout, "skip",
        "no marker = no evidence this harness owns any inactive state -- must never repair on a guess"
    );
}

#[test]
fn decision_skips_on_ambiguous_or_garbage_evidence() {
    for garbage in ["", "2", "maybe", "yes"] {
        let r = run_sourced(&format!("startup_self_heal_decision {garbage:?}"));
        assert_eq!(r.exit_code, 0, "garbage={garbage:?} stderr={}", r.stderr);
        assert_eq!(
            r.stdout, "skip",
            "unrecognized evidence {garbage:?} must conservatively skip, never silently repair"
        );
    }
}

#[test]
fn reason_for_repair_names_the_marker_and_878_family() {
    let r = run_sourced("startup_self_heal_reason 1");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("rig_e2e_marker") && r.stdout.contains("did not clean up"),
        "repair reason must name the marker evidence: {}",
        r.stdout
    );
}

#[test]
fn reason_for_skip_states_the_absence_of_evidence() {
    let r = run_sourced("startup_self_heal_reason 0");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("no evidence"),
        "skip reason must say plainly there is no evidence: {}",
        r.stdout
    );
}

#[test]
fn reason_for_ambiguous_evidence_says_so_explicitly() {
    let r = run_sourced("startup_self_heal_reason maybe");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("ambiguous"),
        "an unrecognized evidence value must be logged as ambiguous, never silently treated as \
         a clean no-evidence skip: {}",
        r.stdout
    );
}

// ---------------------------------------------------------------------------------------------
// (b) static-anchor assertions — scripts/recording-e2e.sh must genuinely wire the step in.
// ---------------------------------------------------------------------------------------------

#[test]
fn harness_sources_the_startup_self_heal_lib() {
    let s = read_harness();
    assert!(
        s.contains("lib/startup-self-heal.sh"),
        "#878: the harness must source scripts/lib/startup-self-heal.sh -- the decision is \
         single-sourced there, never re-implemented inline"
    );
}

/// The startup self-heal call must run BEFORE the existing `[0/8]` fleet preflight text --
/// "before the harness takes anything over" per the #878 design.
#[test]
fn harness_calls_the_decision_before_the_fleet_preflight() {
    let s = read_harness();
    let call = s
        .find("startup_self_heal_decision")
        .expect("#878: recording-e2e.sh must call startup_self_heal_decision");
    let preflight = s
        .find("[0/8] fleet preflight —")
        .expect("#878: expected the existing [0/8] fleet preflight step");
    assert!(
        call < preflight,
        "#878: startup self-heal must run BEFORE the fleet preflight (call at {call}, preflight \
         at {preflight}) -- repairing after the preflight has already asserted state is too late"
    );
}

/// Gated to ALL_CAMBOX — the bug's own scope (only the sweep touches cam3/cam4), matching the
/// existing fleet preflight's own gating.
#[test]
fn harness_gates_the_startup_self_heal_on_all_cambox() {
    let s = read_harness();
    let call = s
        .find("startup_self_heal_decision")
        .expect("#878: recording-e2e.sh must call startup_self_heal_decision");
    let head = &s[..call];
    let guard_at = head
        .rfind(r#"if [ "${ALL_CAMBOX:-0}" = "1" ]; then"#)
        .expect("#878: startup self-heal must be gated on ALL_CAMBOX");
    // Nothing must close that specific `if` between the guard and the call (a `fi` at column 0
    // ends a block) -- otherwise the call would sit OUTSIDE the guard despite one existing
    // earlier in the file.
    let between = &head[guard_at..];
    assert!(
        !between.contains("\nfi\n"),
        "#878: the nearest preceding ALL_CAMBOX guard must still be OPEN at the call site \
         (found an intervening 'fi'): {between}"
    );
}

/// Bounds the #878 block for the content assertions below: starts at the `startup_self_heal_decision`
/// call, ends at the existing `[0/8] fleet preflight —` text (already proven to follow it above).
fn startup_self_heal_block(s: &str) -> &str {
    let start = s
        .find("startup_self_heal_decision")
        .expect("#878: expected the startup_self_heal_decision call");
    let end = s[start..]
        .find("[0/8] fleet preflight —")
        .map(|i| start + i)
        .expect("#878: expected the block to end before the existing fleet preflight text");
    &s[start..end]
}

/// The block must call the `startup_self_heal_cambox_verify_cmds` WRAPPER, not the raw
/// `camera_box_verify_active_cmds` helper name directly. This is deliberate, not incidental:
/// several EXISTING static-anchor tests (tests/harness_recording_e2e_cleanup_verifies_restart_675.rs,
/// tests/harness_recording_e2e_cleanup_final_verify_684.rs) locate cleanup()'s OWN calls to that
/// helper via a plain `.find()`, and a bare second occurrence earlier in the file (this startup
/// step runs before cleanup() is even defined) shadows the real one and breaks them -- reproduced
/// live while building this fix. `startup_self_heal_genuinely_reuses_the_cambox_verify_helper`
/// below proves the wrapper still delegates to the real helper, so this indirection is not a
/// parallel reimplementation.
#[test]
fn startup_self_heal_block_repairs_camera_box_service_via_the_verify_wrapper() {
    let s = read_harness();
    let block = startup_self_heal_block(&s);
    assert!(
        block.contains("startup_self_heal_cambox_verify_cmds"),
        "#878: must call the startup_self_heal_cambox_verify_cmds wrapper (never the bare \
         camera_box_verify_active_cmds name, which would shadow cleanup()'s own #675/#684 calls \
         for the sibling static-anchor tests). Block:\n{block}"
    );
    assert!(
        !block.contains("camera_box_verify_active_cmds"),
        "#878: the block must NOT call camera_box_verify_active_cmds directly by its bare name -- \
         route through the startup_self_heal_cambox_verify_cmds wrapper instead. Block:\n{block}"
    );
}

/// Same reasoning as above, for the painter restore -- must go through
/// `startup_self_heal_painter_verify_cmds`, never the bare `cam2_painter_restore_verify_cmds`
/// name (tests/harness_cam2_painter_provisioning_863.rs locates the real call via an UNBOUNDED
/// `.find()` over the whole file).
#[test]
fn startup_self_heal_block_restores_cam2_painter_via_the_verify_wrapper() {
    let s = read_harness();
    let block = startup_self_heal_block(&s);
    assert!(
        block.contains("startup_self_heal_painter_verify_cmds"),
        "#878: must call the startup_self_heal_painter_verify_cmds wrapper (never the bare \
         cam2_painter_restore_verify_cmds name, which would shadow the real #863 call for \
         tests/harness_cam2_painter_provisioning_863.rs's unbounded .find()). Block:\n{block}"
    );
    assert!(
        !block.contains("cam2_painter_restore_verify_cmds"),
        "#878: the block must NOT call cam2_painter_restore_verify_cmds directly by its bare \
         name -- route through the startup_self_heal_painter_verify_cmds wrapper instead. \
         Block:\n{block}"
    );
}

/// Prove a wrapper genuinely DELEGATES to a real, existing helper rather than reimplementing it --
/// sources both libs together and diffs the wrapper's output against the real helper's own output
/// in a per-test tempdir (never a shared /tmp path, per `.claude/rules/ci-testing-gotchas.md`).
fn assert_wrapper_delegates(real_lib_rel: &str, real_call: &str, wrapper_call: &str) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real_out = tmp.path().join("real.out");
    let wrap_out = tmp.path().join("wrap.out");
    let real_lib = manifest_dir().join(real_lib_rel);
    let harness = format!(
        "set -uo pipefail\n. {real_lib:?}\n. {:?}\n{real_call} > {:?}\n{wrapper_call} > {:?}",
        lib_script(),
        real_out,
        wrap_out,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "harness itself must not crash. stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let real_content = fs::read_to_string(&real_out).expect("read real output");
    let wrap_content = fs::read_to_string(&wrap_out).expect("read wrapper output");
    assert_eq!(
        real_content, wrap_content,
        "the wrapper must produce BYTE-IDENTICAL output to the real helper -- it must delegate, \
         not reimplement"
    );
}

#[test]
fn startup_self_heal_genuinely_reuses_the_cambox_verify_helper() {
    assert_wrapper_delegates(
        "scripts/lib/camera-box-restart-verify.sh",
        "camera_box_verify_active_cmds 'X'",
        "startup_self_heal_cambox_verify_cmds 'X'",
    );
}

#[test]
fn startup_self_heal_genuinely_reuses_the_painter_verify_helper() {
    assert_wrapper_delegates(
        "scripts/lib/cam2-painter-restore-verify.sh",
        "cam2_painter_restore_verify_cmds",
        "startup_self_heal_painter_verify_cmds",
    );
}

#[test]
fn startup_self_heal_block_clears_leaked_burn_via_obs_burn_filter() {
    let s = read_harness();
    let block = startup_self_heal_block(&s);
    assert!(
        block.contains("obs_burn_filter.py") && block.contains("remove"),
        "#878/#844: must clear a leaked genlock_burn via obs_burn_filter.py remove (idempotent, \
         same mechanism cleanup()'s own #246/#257 clear-loop uses). Block:\n{block}"
    );
}

#[test]
fn startup_self_heal_block_derives_fleet_scope_from_camera_active_set_helpers() {
    let s = read_harness();
    let block = startup_self_heal_block(&s);
    assert!(
        block.contains("camera_active_secondary_set") && block.contains("camera_active_excluding"),
        "#878: fleet scope must derive from CAMERA_ACTIVE_SET via the existing \
         camera_active_secondary_set()/camera_active_excluding() helpers -- never a literal \
         cam-number range (.claude/rules/camera-active-set.md). Block:\n{block}"
    );
    for banned in ["for _n in 1 2 3 4", "for _n in 1 2 3 4 5 6 7"] {
        assert!(
            !block.contains(banned),
            "#878: found a literal cam-number range ({banned:?}) instead of deriving from \
             CAMERA_ACTIVE_SET. Block:\n{block}"
        );
    }
}

#[test]
fn startup_self_heal_block_only_acts_when_the_decision_is_repair() {
    let s = read_harness();
    let block = startup_self_heal_block(&s);
    assert!(
        block.contains(r#""repair""#),
        "#878: the block must branch on the decision actually being \"repair\" before touching \
         any box -- never act unconditionally. Block:\n{block}"
    );
}
