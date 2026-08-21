//! issue 1013 — imag-nb offline-ack path. When imag-nb is a KNOWN-ABSENT box (named in
//! `CAMBOX_OFFLINE_ACK` / `rig-fleet.txt`, exactly like a cam box, #758/#827), the E2E harness
//! must NOT abort at minute 0 — it must SKIP the whole imag leg with a loud, honest, report-only
//! note, never a silent pass (the "ONE full test, no partials" doctrine, #798).
//!
//! These guards lock: the new `scripts/lib/imag-offline-ack.sh` pure `imag_leg_skip_note` helper;
//! the extended `scripts/lib/imag-leg-marker.sh` acked-reason marker (its #798 twin); the
//! reachability preflight's imag ack branch (stale-if-reachable / skip-if-unreachable); and that
//! every imag hard-abort site in recording-e2e.sh is guarded by `IMAG_OFFLINE_ACKED`. Tier-0:
//! pure `fs::read_to_string` + source-and-call bash, no OBS/ssh/live rig — mirrors
//! `tests/harness_imag_topology.rs` + `tests/harness_imag_leg_marker_798.rs`.

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source a lib (cwd = crate root, `cargo test`'s cwd — same as every sibling harness) and run one
/// call; return trimmed stdout, asserting the call exits 0 (these helpers are pure/read-only).
fn call(script: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run bash");
    assert!(
        out.status.success(),
        "helper exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------
// 1. scripts/lib/imag-offline-ack.sh — imag_leg_skip_note LABEL REASON.
// ---------------------------------------------------------------------------

#[test]
fn skip_note_is_a_loud_greppable_report_only_line_naming_label_reason_and_ticket() {
    let m = call(
        ". scripts/lib/imag-offline-ack.sh; \
         imag_leg_skip_note \"[4d/8] render-budget gate\" \"notebook-taken-after-event-2026-08-10\"",
    );
    // ONE distinct, greppable token — the imag-leg twin of #758's cam-box NOTE / #798's marker.
    assert!(
        m.starts_with("IMAG-LEG-SKIPPED"),
        "must lead with the distinct greppable token: {m}"
    );
    assert!(
        m.contains("[4d/8] render-budget gate"),
        "must name the step: {m}"
    );
    assert!(
        m.contains("notebook-taken-after-event-2026-08-10"),
        "must name the operator-acked reason: {m}"
    );
    assert!(m.contains("1013"), "must cite the ticket: {m}");
    // Honest, never a silent pass: it must say the leg was SKIPPED and it is report-only.
    let low = m.to_lowercase();
    assert!(low.contains("skip"), "must state the leg is skipped: {m}");
    assert!(
        low.contains("report-only"),
        "must state it is report-only: {m}"
    );
}

#[test]
fn skip_note_defaults_an_empty_reason_without_crashing() {
    let m =
        call(". scripts/lib/imag-offline-ack.sh; imag_leg_skip_note \"[0/8] display-path\" \"\"");
    assert!(
        m.starts_with("IMAG-LEG-SKIPPED"),
        "still emits the token: {m}"
    );
    assert!(
        m.contains("[0/8] display-path"),
        "still names the step: {m}"
    );
}

// ---------------------------------------------------------------------------
// 2. scripts/lib/imag-leg-marker.sh — the optional 3rd arg (acked reason) marker.
// ---------------------------------------------------------------------------

#[test]
fn marker_names_acked_offline_when_given_a_reason() {
    // 3rd arg present -> distinct, honest NOT-VERIFIED reason (a skipped leg is a NAMED partial).
    let m = call(
        ". scripts/lib/imag-leg-marker.sh; \
         imag_leg_run_marker \"\" \"\" \"notebook-taken-after-event-2026-08-10\"",
    );
    assert!(
        m.starts_with("IMAG-LEG-NOT-VERIFIED"),
        "still the #798 token: {m}"
    );
    assert!(
        m.to_lowercase().contains("acked offline"),
        "must name the acked-offline cause, not the generic 'no recording path': {m}"
    );
    assert!(
        m.contains("notebook-taken-after-event-2026-08-10"),
        "must carry the operator reason: {m}"
    );
    assert!(m.contains("1013"), "must cite the ticket: {m}");
}

#[test]
fn marker_798_behaviour_is_unchanged_without_the_third_arg() {
    // Regression: the existing #798 two-arg calls must behave EXACTLY as before.
    let no_path = call(". scripts/lib/imag-leg-marker.sh; imag_leg_run_marker \"\" \"\"");
    assert!(
        no_path.contains("no imag recording path"),
        "798 no-path reason preserved: {no_path}"
    );
    assert!(no_path.contains("798"), "798 citation preserved: {no_path}");
    assert!(
        !no_path.to_lowercase().contains("acked offline"),
        "must NOT claim acked-offline when no reason was passed: {no_path}"
    );

    let extract_failed =
        call(". scripts/lib/imag-leg-marker.sh; imag_leg_run_marker \"\" \"/home/x/imag-REC.mkv\"");
    assert!(
        extract_failed.contains("extract failed"),
        "798 extract-failed reason preserved"
    );
}

// ---------------------------------------------------------------------------
// 3-4. recording-e2e.sh wiring: source, init, reachability ack branch, leg-skip guards.
// ---------------------------------------------------------------------------

fn e2e() -> String {
    read("scripts/recording-e2e.sh")
}

#[test]
fn e2e_sources_the_imag_offline_ack_lib_and_inits_the_gate_flag() {
    let s = e2e();
    assert!(
        s.contains(". \"$HERE/lib/imag-offline-ack.sh\""),
        "recording-e2e.sh must source the new imag-offline-ack lib"
    );
    // A single, explicitly-defaulted early gate variable (issue 1013 Architektúra).
    assert!(
        s.contains("IMAG_OFFLINE_ACKED=0"),
        "must init the IMAG_OFFLINE_ACKED gate flag to 0 explicitly"
    );
}

/// The reachability preflight must give imag (and only imag) an ack-aware branch, reusing the
/// EXISTING cam-box mechanism: reachable+acked -> STALE fail; unreachable+acked -> NOTE + set the
/// gate flag. It must STILL list `imag=$IMAG_IP` (the harness_imag_topology anchor).
#[test]
fn reachability_preflight_has_an_imag_ack_branch() {
    let s = e2e();
    let start = s
        .find("reachability preflight")
        .expect("reachability preflight step must exist");
    let end = start
        + s[start..]
            .find("\ndone\n")
            .expect("the loop must close with `done`")
        + 6;
    let region = &s[start..end];
    // Anchor preserved (harness_imag_topology).
    assert!(
        region.contains("imag=$IMAG_IP"),
        "imag stays in the reachability host list: {region}"
    );
    // Reuse the cam-box ack mechanism (#758/#827) rather than a parallel one.
    assert!(
        region.contains("cambox_offline_ack_is_acked"),
        "imag must consult the existing ack mechanism in the reachability loop: {region}"
    );
    assert!(
        region.contains("cambox_offline_ack_stale_message"),
        "an acked-but-REACHABLE imag must fail as a stale ack: {region}"
    );
    assert!(
        region.contains("IMAG_OFFLINE_ACKED=1"),
        "an acked-and-UNREACHABLE imag must set the gate flag instead of exit 1: {region}"
    );
}

/// `imag_leg_skip_note` / `IMAG_OFFLINE_ACKED` must appear near EACH imag hard-abort site so the
/// whole leg is skipped (not just the reachability loop) — else the run just dies at the next imag
/// gate (issue 1013 Approach 3, rejected). Helper: assert the gate flag is consulted within a
/// window BEFORE a given imag command anchor.
fn guarded_before(s: &str, anchor: &str) {
    let at = s
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor not found: {anchor}"));
    let window_start = at.saturating_sub(1400);
    let window = &s[window_start..at];
    assert!(
        window.contains("IMAG_OFFLINE_ACKED"),
        "imag step `{anchor}` is not guarded by IMAG_OFFLINE_ACKED (would hard-abort when imag is acked-offline)"
    );
}

#[test]
fn every_imag_hard_abort_site_is_guarded_by_the_gate_flag() {
    let s = e2e();
    // [0/8] display-path preflight.
    guarded_before(&s, "imag_display_path_preflight_assert \"$IMAG_IP\"");
    // [0/8] kernel-cmdline isolation preflight (issue 1105 — the issue-784 lib's E2E consumer, a
    // new imag hard-abort site: `… || exit 1`, so it must be guarded to skip cleanly when acked).
    guarded_before(&s, "imag_cmdline_isolation_preflight_assert \"$IMAG_IP\"");
    // ALL_CAMBOX imag OBS-prep (reachability probe / projectors / wmctrl / heal / studio).
    guarded_before(&s, "imag-nb OBS reachability probe");
    // [1/8] imag render-health preflight.
    guarded_before(&s, "imag render-health preflight");
    // [4a/8] imag program-scene routing (bare `switch` under set -e).
    guarded_before(&s, "imag program-scene routing");
    // [4d/8] imag render-budget gate call. NOTE (anchor disambiguation, issue 1013 review 🔵): the
    // SAME `--box "imag=${IMAG_IP}:${RENDER_TARGET_FPS_IMAG:-60}"` string also appears in the [1/8]
    // render-health call — the two are told apart ONLY by the indent of the FOLLOWING `--window-s`
    // line (6 spaces here in [4d/8], 8 in [1/8]). Both calls ARE guarded, so a mis-target still
    // catches a regression — but if a future edit re-indents either call, update this anchor.
    guarded_before(
        &s,
        "--box \"imag=${IMAG_IP}:${RENDER_TARGET_FPS_IMAG:-60}\" \\\n      --window-s",
    );
    // [4e/8] imag-nb headroom preflight.
    guarded_before(&s, "imag-nb headroom preflight");
    // [5/8] imag StartRecord.
    guarded_before(&s, "record --host \"$IMAG_IP\" --action start");
    // [0/8] dantesync version pin gate — hard-aborts (exit 11) on an UNREAD imag; it names imag
    // "imag-nb" (not "imag"), so its own ack-exclusion never matches the "imag" ack — imag-nb must
    // be dropped from DANTESYNC_VERSION_LINUX when acked, else the gate refuses the whole run.
    guarded_before(&s, "imag-nb=${IMAG_USER:-newlevel}@$IMAG_IP");
    // [4b/8] pre-record burn-ON gate — exit 1 when imag's burn can't be confirmed (impossible on an
    // absent box). BURN_TARGETS keeps its imag entry (harness_imag_topology anchor), so the loop
    // itself must skip the imag triple when acked.
    guarded_before(
        &s,
        "burn would be absent from the recording and the run would be wasted",
    );
}

/// The [8/8c] imag-leg marker must be told the acked reason so its run-log line names the true
/// cause (acked offline) rather than the generic #798 "no recording path".
#[test]
fn imag_leg_marker_call_passes_the_acked_reason() {
    let s = e2e();
    let at = s
        .find("imag_leg_run_marker")
        .expect("the marker call must exist");
    let line_end = at + s[at..].find('\n').unwrap_or(0);
    let line = &s[at..line_end];
    assert!(
        line.contains("IMAG_OFFLINE_ACK"),
        "the marker call must forward the acked reason as its 3rd arg: {line}"
    );
}

/// #1164: the [0/8] version-integrity gate is an imag hard-abort site the issue-1013 inventory had
/// missed — after the issue-1100 ENFORCED flip an acked-absent imag (empty SHA + no .so bytes) makes
/// the gate UNKNOWN-refuse the whole E2E. When imag is acked offline, recording-e2e.sh must (1)
/// invoke version-integrity-gate.sh with `--imag-acked-offline "$IMAG_OFFLINE_ACK_REASON"` so the
/// gate skips the imag facets instead of refusing, and (2) guard the imag genlock-SHA ssh read so no
/// ssh is attempted to the acked-absent box.
#[test]
fn version_integrity_gate_is_imag_ack_guarded_1164() {
    let s = e2e();
    assert!(
        s.contains("--imag-acked-offline \"$IMAG_OFFLINE_ACK_REASON\""),
        "#1164: recording-e2e.sh must invoke version-integrity-gate.sh with \
         --imag-acked-offline \"$IMAG_OFFLINE_ACK_REASON\" when imag is acked offline"
    );
    // The imag genlock-SHA ssh read (unique anchor) must be guarded by the gate flag so an
    // acked-absent imag is never sshed (would waste an 8s timeout every run).
    guarded_before(&s, "cat /opt/obs-genlock/GENLOCK_BUILD_SHA.txt");
}
