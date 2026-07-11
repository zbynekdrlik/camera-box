//! #691 — `scripts/recording-e2e.sh`'s #358 delivery-verify step was silently STOMPING the
//! CALIBRATED stream-box A/V-align latency (925ms) back to the test value (1000ms) on every run,
//! including every required merge-gate run — un-syncing lipsync by ~+82ms until a human noticed
//! and manually restored it over the OBS WebSocket. Root cause: `obs_phase2.py`'s
//! `_snapshot_and_set_test_latency` took an "already at target — nothing to force" shortcut that
//! also DISCARDED any existing saved snapshot without restoring it — so once a box got stuck at
//! the test value (e.g. an earlier run's own restore never completing, a crash before cleanup()
//! ran), every SUBSEQUENT run saw `prod_latency == test_latency_ms` and silently perpetuated the
//! stomp forever.
//!
//! ## The fix these tests lock (static read of the shell + python scripts — no rig, no ssh)
//!
//! 1. `_snapshot_and_set_test_latency` now saves the snapshot UNCONDITIONALLY, BEFORE the
//!    "already at target" check — pytest (`tests/python/test_obs_phase2_latency_delivery.py`)
//!    covers the runtime behavior; this file pins the STRUCTURAL bracketing (snapshot save
//!    strictly precedes the early-return) directly in the source, so a future edit can't silently
//!    reorder them back into the bug.
//! 2. `resolve_test_latency_ms` (pure, pytest-covered) derives the EFFECTIVE test latency from the
//!    box's OWN current value when the caller left `GENLOCK_TEST_LATENCY_MS` unset — `recording-
//!    e2e.sh` no longer forces a blind `1000` default.
//! 3. `AV_SYNC_CALIBRATED_MS` (belt-and-braces, OPTIONAL) is declared BEFORE the cleanup trap
//!    installs (same ordering discipline every other cleanup()-referenced config var in this file
//!    already follows) and threaded through to `teardown --host "$STREAM"` conditionally.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn obs_phase2() -> String {
    read("scripts/obs_phase2.py")
}

fn recording_e2e() -> String {
    read("scripts/recording-e2e.sh")
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (same slice every
/// sibling cleanup test uses).
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

// ---------------------------------------------------------------------------------------------
// (1) The snapshot MUST be saved BEFORE the "already at target" short-circuit — the actual bug.
// ---------------------------------------------------------------------------------------------

#[test]
fn snapshot_and_set_test_latency_saves_state_unconditionally_before_the_already_at_target_check() {
    let s = obs_phase2();
    let fn_start = s
        .find("def _snapshot_and_set_test_latency(")
        .expect("obs_phase2.py must define _snapshot_and_set_test_latency");
    let fn_end = s[fn_start..]
        .find("\ndef _restore_test_latency(")
        .map(|i| fn_start + i)
        .expect("_restore_test_latency must follow _snapshot_and_set_test_latency");
    let body = &s[fn_start..fn_end];

    let save_pos = body
        .find("host_state[_TEST_LATENCY_STATE_KEY] = {")
        .expect("#691: the snapshot must be assigned into state[host][_TEST_LATENCY_STATE_KEY]");
    let save_call_pos = body[save_pos..]
        .find("_save_state(state)")
        .map(|i| save_pos + i)
        .expect("#691: the snapshot assignment must be followed by a _save_state(state) call");
    let already_at_target_pos = body
        .find("if prod_latency == test_latency_ms:")
        .expect("#358: the already-at-target comparison must still exist");

    assert!(
        save_call_pos < already_at_target_pos,
        "#691: the snapshot MUST be saved to disk BEFORE the \"already at target\" check -- \
         saving it only in the force-path (the OLD, buggy order) silently destroys the one \
         piece of information that could recover a box a PRIOR run left stuck. Function body:\n{body}"
    );

    // The old bug's exact shape (`.pop(_TEST_LATENCY_STATE_KEY, None)` inside this function,
    // discarding a leftover snapshot instead of using it) must be gone.
    assert!(
        !body.contains(".pop(_TEST_LATENCY_STATE_KEY"),
        "#691: _snapshot_and_set_test_latency must never discard an existing snapshot -- that \
         was the root cause of the permanent stomp. Function body:\n{body}"
    );
}

// ---------------------------------------------------------------------------------------------
// (2) resolve_test_latency_ms exists and is actually WIRED into the snapshot/set path.
// ---------------------------------------------------------------------------------------------

#[test]
fn resolve_test_latency_ms_is_defined_and_used_by_snapshot_and_set() {
    let s = obs_phase2();
    assert!(
        s.contains("def resolve_test_latency_ms("),
        "#691: obs_phase2.py must define the pure resolve_test_latency_ms decision function"
    );
    let fn_start = s
        .find("def _snapshot_and_set_test_latency(")
        .expect("obs_phase2.py must define _snapshot_and_set_test_latency");
    let fn_end = s[fn_start..]
        .find("\ndef _restore_test_latency(")
        .map(|i| fn_start + i)
        .unwrap();
    let body = &s[fn_start..fn_end];
    assert!(
        body.contains("resolve_test_latency_ms(requested_test_latency_ms, prod_latency)"),
        "#691: _snapshot_and_set_test_latency must resolve the EFFECTIVE test latency via \
         resolve_test_latency_ms, not just use whatever the caller passed verbatim. Body:\n{body}"
    );
}

/// The CLI default for --test-latency-ms must no longer force a literal 1000 -- it now falls
/// back to None (auto-derive) unless GENLOCK_TEST_LATENCY_MS is explicitly set.
#[test]
fn cli_test_latency_ms_default_is_env_or_none_not_a_forced_1000() {
    let s = obs_phase2();
    let arg_pos = s
        .find("\"--test-latency-ms\"")
        .expect("obs_phase2.py must still define the --test-latency-ms CLI arg");
    let window = &s[arg_pos..(arg_pos + 300).min(s.len())];
    assert!(
        window.contains("_int_env_or_none(\"GENLOCK_TEST_LATENCY_MS\")"),
        "#691: --test-latency-ms's default must resolve via _int_env_or_none (None when unset), \
         not a hardcoded int(...) default of 1000. Window:\n{window}"
    );
    assert!(
        !window.contains("int(os.environ.get(\"GENLOCK_TEST_LATENCY_MS\", \"1000\"))"),
        "#691: the OLD forced-1000 default must be gone. Window:\n{window}"
    );
}

// ---------------------------------------------------------------------------------------------
// (3) AV_SYNC_CALIBRATED_MS declared BEFORE the cleanup trap; threaded through conditionally.
// ---------------------------------------------------------------------------------------------

#[test]
fn av_sync_calibrated_ms_is_declared_before_the_cleanup_trap() {
    let s = recording_e2e();
    let decl_pos = s
        .find("AV_SYNC_CALIBRATED_MS=\"${AV_SYNC_CALIBRATED_MS:-}\"")
        .expect(
            "#691: recording-e2e.sh must declare AV_SYNC_CALIBRATED_MS with a safe empty \
             default (mirrors every other cleanup()-referenced config var in this file)",
        );
    let trap_pos = s
        .find("\ntrap cleanup EXIT HUP INT TERM")
        .expect("recording-e2e.sh must install the cleanup trap");
    assert!(
        decl_pos < trap_pos,
        "#691: AV_SYNC_CALIBRATED_MS must be declared BEFORE `trap cleanup EXIT ...` installs -- \
         cleanup() reads it, so an early abort before this line would otherwise `set -u`-abort \
         the trap (same ordering reason every *_PROG_SOURCE var precedes the trap)."
    );
}

#[test]
fn cleanup_passes_calibrated_latency_ms_to_stream_teardown_only_when_set() {
    let body = cleanup_body(&recording_e2e());
    assert!(
        body.contains("_stream_teardown_args=(teardown --host \"$STREAM\")"),
        "#691: cleanup() must build the stream teardown call as an args array so \
         --calibrated-latency-ms can be added conditionally. Body:\n{body}"
    );
    assert!(
        body.contains("if [ -n \"$AV_SYNC_CALIBRATED_MS\" ]; then"),
        "#691: cleanup() must only add --calibrated-latency-ms when AV_SYNC_CALIBRATED_MS is \
         actually set (empty by default -- the common unattended-CI case must not pass an \
         empty/invalid value to obs_phase2.py's int argument). Body:\n{body}"
    );
    assert!(
        body.contains("--calibrated-latency-ms \"$AV_SYNC_CALIBRATED_MS\""),
        "#691: cleanup() must pass --calibrated-latency-ms through when set. Body:\n{body}"
    );
    // The STRIH teardown call is unaffected -- only the STREAM box carries the A/V-align latency.
    assert!(
        body.contains("teardown --host \"$STRIH\""),
        "#691: the strih teardown call must remain a plain (non-array) call -- unaffected by \
         this fix. Body:\n{body}"
    );
}

/// `--calibrated-latency-ms` must exist as a real CLI arg on the `teardown` subcommand, resolved
/// from the OPTIONAL AV_SYNC_CALIBRATED_MS env var (never a hard requirement).
#[test]
fn cli_calibrated_latency_ms_arg_exists_on_teardown_and_is_optional() {
    let s = obs_phase2();
    let arg_pos = s
        .find("\"--calibrated-latency-ms\"")
        .expect("#691: obs_phase2.py must define --calibrated-latency-ms");
    let window = &s[arg_pos..(arg_pos + 200).min(s.len())];
    assert!(
        window.contains("_int_env_or_none(\"AV_SYNC_CALIBRATED_MS\")"),
        "#691: --calibrated-latency-ms's default must be OPTIONAL (None when unset), resolved \
         from AV_SYNC_CALIBRATED_MS. Window:\n{window}"
    );
}

/// `teardown()` must actually forward the CLI value into `_restore_test_latency` -- defining the
/// arg without wiring it through would be a silent no-op.
#[test]
fn teardown_forwards_calibrated_latency_ms_into_restore_test_latency() {
    let s = obs_phase2();
    let fn_start = s
        .find("def teardown(a):")
        .expect("obs_phase2.py must define teardown(a)");
    let fn_end = s[fn_start..]
        .find("\ndef ")
        .map(|i| fn_start + i)
        .unwrap_or(s.len());
    let body = &s[fn_start..fn_end];
    assert!(
        body.contains("getattr(a, \"calibrated_latency_ms\", None)"),
        "#691: teardown() must forward a.calibrated_latency_ms into _restore_test_latency. \
         Body:\n{body}"
    );
}
