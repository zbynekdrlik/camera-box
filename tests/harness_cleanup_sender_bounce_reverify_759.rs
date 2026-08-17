//! #759 — wire the #758 item 2 sender-bounce re-verify into `cleanup()`'s restore blocks (the
//! deploy-time half already shipped in #758). This is the cleanup half: after `cleanup()`'s
//! device-restore phase restarts camera-box on every active box (a sender bounce), re-verify each
//! box's "NDI camN" leg re-locked on strih and nudge it once (WARN-only reattach) if not — so a
//! restart-left-unlocked leg never poisons the NEXT run's `[0/8]` preflight.
//!
//! Two hazards the #758 comment named, locked here as static-text assertions (same read-only
//! discipline as every sibling `harness_recording_e2e_*` cleanup test — no rig, no ssh):
//!
//!   1. FUNCTION-ORDERING: `cleanup()` (armed at `trap cleanup EXIT HUP INT TERM`) can fire from
//!      ANY exit path, including a `[0/8]` preflight failure BEFORE `preflight_mv_reverify()` (at
//!      line 1702 pre-#759) was ever defined. So its definition MUST move ABOVE the trap, or a
//!      call from inside the trap is a "command not found".
//!   2. TRAP-SAFETY: the cleanup re-verify must be WARN-only (`|| echo`, never `exit`) — unlike
//!      the deploy-time `|| exit 1` sites — and must never fire against an unset `$PROBE_BIN_DIR`
//!      (cleanup can run before it is set), so it guards on the frozen-camera-gate binary existing.

use std::fs;
use std::path::PathBuf;

fn recording_e2e() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of `cleanup()` — from `cleanup() {` (the real function definition, not the bare
/// substring that matches a prose comment ~1000 lines earlier, per the #758 fix in
/// harness_recording_e2e_cleanup_resilient.rs) to the `\ntrap ` that installs it.
fn cleanup_body(s: &str) -> &str {
    let start = s
        .find("cleanup() {")
        .expect("recording-e2e.sh must define cleanup() {");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    &s[start..end]
}

/// Extract a flat `NAME() { ... }` snippet — header `"NAME() {"` to the next top-level `"\n}\n"`.
/// The reverify helpers are flat functions (no nested braces on their own lines), so this is exact.
fn function_body<'a>(s: &'a str, name: &str) -> &'a str {
    let header = format!("{name}() {{");
    let start = s
        .find(&header)
        .unwrap_or_else(|| panic!("expected recording-e2e.sh to define {name}()"));
    let rel_end = s[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("expected {name}() to close with a top-level }}"));
    &s[start..start + rel_end + "\n}".len()]
}

/// HAZARD 1: `preflight_mv_reverify()` must now be DEFINED before the cleanup trap is armed — so a
/// call to it from inside `cleanup()` (which the trap can fire at any point) is always callable.
#[test]
fn preflight_mv_reverify_is_defined_before_the_cleanup_trap() {
    let s = recording_e2e();
    let def = s
        .find("preflight_mv_reverify() {")
        .expect("#759: preflight_mv_reverify must be defined");
    let trap = s
        .find("\ntrap cleanup EXIT")
        .expect("#759: the cleanup trap must be armed");
    assert!(
        def < trap,
        "#759: preflight_mv_reverify() must be defined BEFORE the cleanup trap is armed \
         (def@{def} vs trap@{trap}) — cleanup() can fire from an early [0/8] preflight failure \
         before a definition further down the file ever executes"
    );
}

/// The dedicated WARN-only cleanup wrapper must exist and be defined before the trap too.
#[test]
fn cleanup_mv_reverify_wrapper_is_defined_before_the_trap() {
    let s = recording_e2e();
    let def = s
        .find("cleanup_mv_reverify_active_boxes() {")
        .expect("#759: cleanup_mv_reverify_active_boxes() must be defined");
    let trap = s
        .find("\ntrap cleanup EXIT")
        .expect("#759: the cleanup trap must be armed");
    assert!(
        def < trap,
        "#759: the cleanup reverify wrapper must be defined before the trap"
    );
}

/// `cleanup()` must CALL the wrapper, and only AFTER the device-restore phase's shared
/// `cambox_parallel_wait_and_report` (the boxes must have finished restarting before we re-verify).
#[test]
fn cleanup_calls_the_reverify_wrapper_after_the_restart_wait() {
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let wait = body
        .find("cambox_parallel_wait_and_report")
        .expect("#759: cleanup() must wait for the parallel device restore");
    let call = body
        .find("\n  cleanup_mv_reverify_active_boxes")
        .expect("#759: cleanup() must CALL cleanup_mv_reverify_active_boxes in its restore phase");
    assert!(
        wait < call,
        "#759: the sender-bounce reverify must run AFTER the boxes finished restarting \
         (wait@{wait} vs call@{call})"
    );
}

/// TRAP-SAFETY: the wrapper must be WARN-only — it must NEVER `exit` (a cleanup-trap abort is the
/// #328/#712/#713 footgun this whole cleanup convention forbids). It must also carry the two
/// guards the #758 comment demanded (ALL_CAMBOX only; a `$PROBE_BIN_DIR/frozen-camera-gate`
/// readiness check so it never fires against an unset PROBE_BIN_DIR), iterate the active fleet,
/// and bound the per-box budget to a single check + one fire-and-forget reattach.
#[test]
fn cleanup_reverify_wrapper_is_warn_only_and_guarded() {
    let s = recording_e2e();
    let wrapper = function_body(&s, "cleanup_mv_reverify_active_boxes");

    assert!(
        !wrapper.contains("exit "),
        "#759: the cleanup reverify wrapper must NEVER exit — it runs inside the EXIT trap and \
         must always let cleanup() complete (WARN-only). Wrapper:\n{wrapper}"
    );
    assert!(
        wrapper.contains(r#"[ "${ALL_CAMBOX:-0}" = "1" ]"#),
        "#759: the wrapper must no-op unless ALL_CAMBOX=1 (matches preflight_mv_reverify's own \
         guard + the restart set it follows). Wrapper:\n{wrapper}"
    );
    assert!(
        wrapper.contains(r#"-x "${PROBE_BIN_DIR:-}/frozen-camera-gate""#),
        "#759: the wrapper must guard on the frozen-camera-gate binary existing, so it never \
         fires against an unset $PROBE_BIN_DIR when cleanup runs before deploy setup. Wrapper:\n{wrapper}"
    );
    assert!(
        wrapper.contains("$CAMERA_ACTIVE_SET"),
        "#759: the wrapper must reverify every ACTIVE camera (derived from CAMERA_ACTIVE_SET, the \
         #827 single source of truth), never a hardcoded list. Wrapper:\n{wrapper}"
    );
    assert!(
        wrapper.contains("PREFLIGHT_MV_REVERIFY_ATTEMPTS=1"),
        "#759: the cleanup budget must be bounded to a single check + one fire-and-forget reattach \
         per box (attempts=1) — never the deploy-time multi-attempt settle loop that could outlast \
         a GH-Actions cancellation grace window. Wrapper:\n{wrapper}"
    );
    assert!(
        wrapper.contains("preflight_mv_reverify \"$_rvcam\" \"${_rvcam#cam}\""),
        "#759: the wrapper must reuse the shared preflight_mv_reverify() (single source of truth), \
         not a duplicated inline reverify. Wrapper:\n{wrapper}"
    );
}
