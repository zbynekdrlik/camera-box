//! #1076 — preserve the alert dedup signature on an UNMEASURED (unknown) pass, uniformly across the
//! three alert paths of `scripts/imag-power-envelope-alert-watchdog.sh`.
//!
//! Root cause: each path (`alert_from_journal` / `alert_from_throttle` /
//! `alert_from_render_discriminator`) owns its own dedup state (`alert_*` / `throttle_*` /
//! `render_*`) and pages via the shared `obs_watchdog_alert_throttle`, which re-pages whenever the
//! current signature differs from the prior one. On a pass where a path's INPUT is UNMEASURED (an
//! ssh hiccup emptying `JOURNAL`; a truncated/failed `BURST`; a failed OBS-WS `RENDER` read → the
//! `unknown` cause), the current code writes `sig=""` + `passes=0` — treating "could not measure"
//! as "the episode resolved". So one transient measurement gap during ONE ongoing episode wipes the
//! "already paged" memory and the next measured pass re-pages, defeating the ~1h throttle.
//!
//! The fix treats "unmeasured" as "no new information": PRESERVE the dedup signature + pass count on
//! an unmeasured pass (resetting only the render path's per-candidate confirm counter), while a
//! genuinely MEASURED-healthy pass still resets (the episode really resolved → a future one pages
//! fresh). This file pins that contract uniformly for all three paths.
//!
//! Tier-0: sources the REAL watchdog script (defining the `alert_from_*` functions without running
//! `main`), seeds a per-test `tempfile::tempdir()` state file, drives one alert function with a
//! chosen input, and reads the resulting state back. RED before the fix (the seeded sig/passes are
//! wiped on an unmeasured pass); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/imag-power-envelope-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Wrap an arbitrary string as a single bash-safe single-quoted argument.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Source the watchdog (defines `alert_from_*` without running `main`, since `BASH_SOURCE[0]` != $0
/// when sourced), seed the state file with `seed`, run `vars` (setting the per-path input vars) and
/// then `call` (one alert function, in dry-run so nothing pages), and return the resulting state
/// file text. A per-test `tempfile::tempdir()` state path (never the real `/tmp` default) keeps
/// parallel `cargo test` threads from racing on one file (#975).
fn drive(seed: &str, vars: &str, call: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("imag-power-alert.state");
    let body = format!(
        "set -uo pipefail\n\
         . \"$WD\"\n\
         DRY_RUN=1\n\
         printf '%s' {seed} > \"$STATE_FILE\"\n\
         {vars}\n\
         {call} >/dev/null 2>&1 || true\n\
         cat \"$STATE_FILE\"\n",
        seed = shell_quote(seed),
        vars = vars,
        call = call,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("WD", watchdog())
        .env("IMAG_POWER_ALERT_STATE_FILE", &state)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run watchdog dedup harness");
    assert!(
        out.status.success(),
        "harness bash exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Read the last `key=` value out of a state-file dump (`write_state_field` keeps one line per key).
fn field(state: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    state
        .lines()
        .filter(|l| l.starts_with(&prefix))
        .last()
        .map(|l| l[prefix.len()..].to_string())
        .unwrap_or_else(|| format!("<{key} ABSENT>"))
}

// A CLEAN burst: valid FLOOR, >= min (6) samples, NONE clamped (act at/above floor, throttle all 0)
// = the GPU has headroom = a MEASURED-healthy `clean` state (not `unknown`).
const CLEAN_BURST: &str = "FLOOR|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1350\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1350\n";

// -------------------------------------------------------------------------------------------------
// Path 1 — alert_from_journal (dedup keys alert_sig / alert_passes)
// -------------------------------------------------------------------------------------------------

#[test]
fn journal_unmeasured_empty_journal_preserves_dedup_signature_1076() {
    // An empty JOURNAL is UNMEASURED (an ssh/connectivity hiccup, OR a quiet journal window — the
    // guard logs only on transitions, so a quiet window carries no reading). It must NOT be treated
    // as "the episode resolved": preserve the dedup signature + passes so a persistent episode stays
    // deduped across the transient gap.
    let s = drive(
        "alert_sig=imag-power:STEP-DOWN TCPU=88C\nalert_passes=3\n",
        "JOURNAL=''\nSNAPSHOT=''",
        "alert_from_journal",
    );
    assert_eq!(
        field(&s, "alert_sig"),
        "imag-power:STEP-DOWN TCPU=88C",
        "empty JOURNAL is UNMEASURED -> preserve the dedup signature, never wipe it (#1076): {s:?}"
    );
    assert_eq!(
        field(&s, "alert_passes"),
        "3",
        "empty JOURNAL is UNMEASURED -> preserve the pass count (#1076): {s:?}"
    );
}

#[test]
fn journal_measured_healthy_no_markers_still_resets_1076() {
    // A NON-empty journal with no STEP-DOWN/RE-ASSERT marker (e.g. a RESTORE recovery line) is a
    // MEASURED-healthy reading — the episode genuinely resolved — so the dedup state must still
    // reset, exactly as before, so a later new episode pages fresh. (Guards that the fix does not
    // over-preserve.)
    let s = drive(
        "alert_sig=imag-power:STEP-DOWN TCPU=88C\nalert_passes=3\n",
        "JOURNAL='Aug 17 10:00:00 imag imag-power-envelope: RESTORE: TCPU=70C < 85C sustained'\nSNAPSHOT=''",
        "alert_from_journal",
    );
    assert_eq!(
        field(&s, "alert_sig"),
        "",
        "a measured-healthy journal (present, no STEP-DOWN/RE-ASSERT) resolves the episode -> reset the sig: {s:?}"
    );
    assert_eq!(field(&s, "alert_passes"), "0", "measured-healthy -> reset passes: {s:?}");
}

// -------------------------------------------------------------------------------------------------
// Path 2 — alert_from_throttle (dedup keys throttle_sig / throttle_passes)
// -------------------------------------------------------------------------------------------------

#[test]
fn throttle_unmeasured_unknown_burst_preserves_dedup_signature_1076() {
    // An unreadable burst (empty, or fewer than the min samples) is UNMEASURED
    // (imag_power_throttle_state == unknown), NOT healthy — preserve the dedup state so a persistent
    // clamp stays deduped across a transient ssh/burst gap.
    let s = drive(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=4\n",
        "BURST=''",
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "imag-throttle:under-floor",
        "empty (unknown) burst is UNMEASURED -> preserve the dedup signature (#1076): {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_passes"),
        "4",
        "empty (unknown) burst is UNMEASURED -> preserve the pass count (#1076): {s:?}"
    );

    // A truncated burst (2 samples < the min of 6) is ALSO unknown -> also preserved.
    let s2 = drive(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=4\n",
        "BURST='FLOOR|1400\nTHROTSAMPLE|1|0|1|700\nTHROTSAMPLE|1|0|1|750\n'",
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s2, "throttle_sig"),
        "imag-throttle:under-floor",
        "a truncated burst (< min samples) is UNMEASURED -> preserve the dedup signature (#1076): {s2:?}"
    );
    assert_eq!(field(&s2, "throttle_passes"), "4", "truncated burst -> preserve passes: {s2:?}");
}

#[test]
fn throttle_measured_clean_burst_still_resets_1076() {
    // A CLEAN burst (>= min samples, FLOOR present, no clamp) is MEASURED-healthy (state == clean),
    // NOT unknown — the GPU has headroom, so the dedup state must still reset. (Guards that the fix
    // does not over-preserve a genuinely-resolved clamp.)
    let s = drive(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=4\n",
        &format!("BURST={}", shell_quote(CLEAN_BURST)),
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "",
        "a clean measured burst (GPU headroom) resolves the clamp -> reset the sig: {s:?}"
    );
    assert_eq!(field(&s, "throttle_passes"), "0", "clean measured burst -> reset passes: {s:?}");
}

// -------------------------------------------------------------------------------------------------
// Path 3 — alert_from_render_discriminator (dedup keys render_sig / render_passes / render_confirm)
// -------------------------------------------------------------------------------------------------

#[test]
fn render_unmeasured_unknown_cause_preserves_sig_and_passes_but_resets_confirm_1076() {
    // An unreadable render sample (WS read failed -> RENDER empty -> cause `unknown`) is UNMEASURED —
    // the ticket's headline "a persistent churn leak whose WS read intermittently fails" case.
    // PRESERVE the dedup signature + passes (so the persistent churn stays deduped), but RESET the
    // confirm counter (a churn candidate cannot carry its 2-pass confirmation across a measurement
    // gap).
    let s = drive(
        "render_sig=imag-render:churn-leak\nrender_passes=5\nrender_confirm=1\n",
        "RENDER=''\nBURST=''",
        "alert_from_render_discriminator",
    );
    assert_eq!(
        field(&s, "render_sig"),
        "imag-render:churn-leak",
        "unknown render cause is UNMEASURED -> preserve the dedup signature (#1076): {s:?}"
    );
    assert_eq!(
        field(&s, "render_passes"),
        "5",
        "unknown render cause is UNMEASURED -> preserve the pass count (#1076): {s:?}"
    );
    assert_eq!(
        field(&s, "render_confirm"),
        "0",
        "unknown render cause -> reset the confirm counter (no confirm across a measurement gap): {s:?}"
    );
}

#[test]
fn render_measured_healthy_cause_still_resets_all_1076() {
    // A MEASURED-healthy render sample -> cause `healthy` -> the episode genuinely resolved -> reset
    // the full dedup state (sig + passes + confirm), exactly as before. (Guards that the fix keeps
    // resetting on a real non-churn measured outcome.)
    let s = drive(
        "render_sig=imag-render:churn-leak\nrender_passes=5\nrender_confirm=1\n",
        "RENDER='RENDER|60.00|9.00|0.000|true'\nBURST=''",
        "alert_from_render_discriminator",
    );
    assert_eq!(field(&s, "render_sig"), "", "measured-healthy render -> reset the sig: {s:?}");
    assert_eq!(field(&s, "render_passes"), "0", "measured-healthy render -> reset passes: {s:?}");
    assert_eq!(field(&s, "render_confirm"), "0", "measured-healthy render -> reset confirm: {s:?}");
}
