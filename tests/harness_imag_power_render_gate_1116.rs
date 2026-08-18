//! #1116 — stop the imag-power-envelope watchdog from spamming Discord: (1) clear-hysteresis on the
//! throttle-under-floor dedup so a chronically FLAPPING clamp is ONE episode, not a fresh page on
//! every re-onset; (2) gate the throttle-under-floor PAGE on the render discriminator so a clamp
//! that is NOT degrading OBS render is log-only, never a Discord page.
//!
//! Root cause (traced in `scripts/imag-power-envelope-alert-watchdog.sh`):
//!  1. `alert_from_throttle` resets `throttle_sig=""` / `throttle_passes=0` on a SINGLE
//!     measured-`clean` pass. The iGPU sits chronically at the issue-1043 PL1-clamped floor, so
//!     `imag_power_throttle_state` flaps clean<->clamped across 5-min passes; each clamped re-onset
//!     after one clean pass sees `prior_sig=""` and pages immediately, defeating the designed ~1h
//!     throttle (`ALERT_THROTTLE_PASSES=12`). Live journal: 141 fires in 7 days.
//!  2. `alert_from_throttle` carries no render signal — it pages whenever the burst is
//!     majority-clamped even when OBS render is provably within the 60fps budget (the "page carries
//!     no actionable signal" case).
//!
//! The fix adds two PURE decisions (Tier-0 unit-testable by sourcing):
//!  - `obs_watchdog_clear_hysteresis <pass_class> <prior_clear_passes> [clear_n]` in
//!    `scripts/lib/obs-watchdog-decision.sh` — clears the dedup signature only after N consecutive
//!    measured-healthy passes; an `episode` pass resets the healthy streak; an `unmeasured` pass
//!    advances nothing (preserves the issue-1076 contract).
//!  - `imag_power_throttle_render_gate <render_line>` in `scripts/lib/imag-power-envelope.sh` —
//!    `page` unless the shared `imag_render_degraded_from_sample` reads a clean `healthy` 60fps
//!    render; fail-open (`page`) on `stalled`/`unknown`/unreadable render (never silent).
//!
//! Tier-0: pure-lib tests SOURCE the real libs; integration tests SOURCE the real watchdog (its
//! `BASH_SOURCE[0]`!=$0 guard skips `main`), seed a per-test `tempfile::tempdir()` state file, drive
//! one alert function, and read back either the state file (dry-run) or a stubbed-notify recorder
//! (page-vs-log-only). RED before the fix; GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn imag_lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/imag-power-envelope.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}
fn ts_lib() -> PathBuf {
    manifest_dir().join("scripts/lib/timesync-authority.sh")
}
fn obs_lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/obs-watchdog-decision.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
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

/// Source the imag lib (+ timesync-authority.sh, whose generic helpers its verdict path reuses) and
/// run `body` against its pure functions. Returns (exit_code, stdout).
fn run_imag(body: &str) -> (i32, String) {
    let harness = format!(
        "set -uo pipefail\n. \"$TSLIB\"\n. \"$LIB\"\n{body}",
        body = body
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", imag_lib())
        .env("TSLIB", ts_lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run imag lib harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Source the obs-watchdog-decision lib and run `body` against its pure functions.
fn run_obs(body: &str) -> (i32, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}", body = body);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", obs_lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run obs-watchdog lib harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Source the watchdog (defines `alert_from_*` without running `main`), seed the state file, set the
/// per-path input vars, run `call` in DRY-RUN (nothing pages), and return the resulting state text.
fn drive_state(seed: &str, vars: &str, call: &str) -> String {
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
        .expect("failed to run watchdog state harness");
    assert!(
        out.status.success(),
        "harness bash exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Source the watchdog, seed state, set input vars, run `call` in NON-dry-run with `airuleset.py
/// notify` stubbed by a recorder script, and return the recorder file text (empty = no page fired).
fn drive_notify(seed: &str, vars: &str, call: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("imag-power-alert.state");
    let rec = dir.path().join("notify.rec");
    let stub = dir.path().join("notify_stub.py");
    std::fs::write(
        &stub,
        "import sys, os\n\
         p = os.environ.get('REC_FILE')\n\
         if p:\n\
         \x20   open(p, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
    )
    .unwrap();
    let body = format!(
        "set -uo pipefail\n\
         . \"$WD\"\n\
         DRY_RUN=0\n\
         printf '%s' {seed} > \"$STATE_FILE\"\n\
         {vars}\n\
         {call} >/dev/null 2>&1 || true\n",
        seed = shell_quote(seed),
        vars = vars,
        call = call,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("WD", watchdog())
        .env("IMAG_POWER_ALERT_STATE_FILE", &state)
        .env("AIRULESET_NOTIFY", &stub)
        .env("REC_FILE", &rec)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run watchdog notify harness");
    assert!(
        out.status.success(),
        "harness bash exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&rec).unwrap_or_default()
}

/// Read the last `key=` value out of a state-file dump.
fn field(state: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    state
        .lines()
        .rfind(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].to_string())
        .unwrap_or_else(|| format!("<{key} ABSENT>"))
}

// A CLEAN burst: valid FLOOR, >= min (6) samples, NONE clamped -> measured-healthy `clean` state.
const CLEAN_BURST: &str = "FLOOR|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1350\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1400\n\
THROTSAMPLE|0|0|0|1350\n";

// A CLAMPED burst: valid FLOOR, >= min samples, MAJORITY power-clamped below the floor -> a
// THROTTLE-UNDER-FLOOR marker fires (imag_power_throttle_state == clamped).
const CLAMPED_BURST: &str = "FLOOR|1400\n\
THROTSAMPLE|1|0|1|700\n\
THROTSAMPLE|1|0|1|720\n\
THROTSAMPLE|1|0|1|680\n\
THROTSAMPLE|1|0|1|700\n\
THROTSAMPLE|1|0|1|750\n\
THROTSAMPLE|1|0|1|690\n\
THROTSAMPLE|1|0|1|710\n\
THROTSAMPLE|1|0|1|700\n";

const HEALTHY_RENDER: &str = "RENDER|60.00|9.00|0.000|true";
const DEGRADED_RENDER: &str = "RENDER|45.00|22.00|0.000|true";
const STALLED_RENDER: &str = "RENDER|60.00|9.00|0.000|false";

// =================================================================================================
// Part A — pure decision: imag_power_throttle_render_gate (scripts/lib/imag-power-envelope.sh)
// =================================================================================================

#[test]
fn render_gate_healthy_render_is_log_only_1116() {
    // A clamp while OBS render is within the 60fps budget is a chronic hardware condition with no
    // actionable signal -> log-only, never a page. (This is the spam case.)
    let (_c, out) = run_imag(&format!(
        "imag_power_throttle_render_gate {}",
        shell_quote(HEALTHY_RENDER)
    ));
    assert_eq!(
        out.trim(),
        "log-only",
        "render healthy -> log-only (no page): {out:?}"
    );
}

#[test]
fn render_gate_degraded_render_pages_1116() {
    // A clamp WHILE render is degraded is the genuine silent-judder case -> page.
    let (_c, out) = run_imag(&format!(
        "imag_power_throttle_render_gate {}",
        shell_quote(DEGRADED_RENDER)
    ));
    assert_eq!(out.trim(), "page", "render degraded -> page: {out:?}");
}

#[test]
fn render_gate_fails_open_on_stalled_and_unreadable_render_1116() {
    // Fail-open: a stalled render, an unreadable/empty render, or a malformed line must PAGE — the
    // gate must never SILENTLY suppress a clamp alert on a measurement gap.
    for r in [STALLED_RENDER, "", "garbage", "RENDER|x|y|z|true"] {
        let (_c, out) = run_imag(&format!(
            "imag_power_throttle_render_gate {}",
            shell_quote(r)
        ));
        assert_eq!(
            out.trim(),
            "page",
            "non-healthy/unreadable render ({r:?}) must fail-open to page: {out:?}"
        );
    }
}

// =================================================================================================
// Part B — pure decision: obs_watchdog_clear_hysteresis (scripts/lib/obs-watchdog-decision.sh)
// =================================================================================================

fn hyst(cls: &str, prior: &str, n: &str) -> (String, String) {
    let (_c, out) = run_obs(&format!("obs_watchdog_clear_hysteresis {cls} {prior} {n}"));
    let action = out
        .lines()
        .find_map(|l| l.strip_prefix("action="))
        .unwrap_or("<none>")
        .to_string();
    let passes = out
        .lines()
        .find_map(|l| l.strip_prefix("clear_passes="))
        .unwrap_or("<none>")
        .to_string();
    (action, passes)
}

#[test]
fn hysteresis_episode_pass_keeps_and_resets_the_healthy_streak_1116() {
    // An active clamp pass never clears; it resets the consecutive-healthy streak to 0.
    let (action, passes) = hyst("episode", "7", "12");
    assert_eq!(action, "keep", "episode -> keep");
    assert_eq!(passes, "0", "episode -> healthy streak reset to 0");
}

#[test]
fn hysteresis_healthy_below_threshold_keeps_and_increments_1116() {
    // A single (or sub-threshold run of) measured-healthy passes must NOT clear — this is the
    // flap-reset fix: it increments the streak and keeps the dedup signature.
    let (action, passes) = hyst("healthy", "0", "12");
    assert_eq!(
        action, "keep",
        "1st healthy pass (0->1 of 12) must keep, not clear"
    );
    assert_eq!(passes, "1", "healthy streak advances to 1");

    let (action2, passes2) = hyst("healthy", "5", "12");
    assert_eq!(action2, "keep", "6th healthy pass still below 12 -> keep");
    assert_eq!(passes2, "6", "healthy streak advances to 6");
}

#[test]
fn hysteresis_clears_only_after_n_consecutive_healthy_passes_1116() {
    // Once N consecutive healthy passes are reached, the episode genuinely resolved -> clear + reset.
    let (action, passes) = hyst("healthy", "11", "12");
    assert_eq!(action, "clear", "12th consecutive healthy pass -> clear");
    assert_eq!(passes, "0", "clear resets the streak");
}

#[test]
fn hysteresis_unmeasured_pass_advances_nothing_1116() {
    // An UNMEASURED pass carries no new information (issue 1076): keep the signature AND leave the
    // healthy streak unchanged (it must not count toward clearing).
    let (action, passes) = hyst("unmeasured", "4", "12");
    assert_eq!(action, "keep", "unmeasured -> keep");
    assert_eq!(
        passes, "4",
        "unmeasured -> healthy streak unchanged (advances nothing)"
    );
}

// =================================================================================================
// Part C — integration: alert_from_throttle flap-reset -> hysteresis (dry-run, state readback)
// =================================================================================================

#[test]
fn throttle_single_clean_pass_preserves_dedup_signature_not_flap_reset_1116() {
    // THE FLAP-RESET BUG: one measured-clean pass during a chronically-flapping clamp must NOT wipe
    // the dedup signature (which would let the next clamp re-onset page immediately). It preserves
    // the signature and advances the consecutive-healthy streak to 1.
    let s = drive_state(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=3\n",
        &format!("BURST={}", shell_quote(CLEAN_BURST)),
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "imag-throttle:under-floor",
        "one clean pass must PRESERVE the dedup signature (flap-reset fix): {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_passes"),
        "3",
        "one clean pass must PRESERVE the pass count while within the hysteresis window: {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_clear_passes"),
        "1",
        "one clean pass advances the consecutive-healthy streak to 1: {s:?}"
    );
}

#[test]
fn throttle_clears_dedup_only_after_full_hysteresis_window_1116() {
    // After N consecutive clean passes (seed the streak at 11, drive the 12th), the clamp is
    // genuinely resolved -> the dedup signature clears so a later NEW clamp pages fresh.
    let s = drive_state(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=3\nthrottle_clear_passes=11\n",
        &format!("BURST={}", shell_quote(CLEAN_BURST)),
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "",
        "the 12th consecutive clean pass resolves the episode -> clear the sig: {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_passes"),
        "0",
        "resolved -> reset passes: {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_clear_passes"),
        "0",
        "clearing resets the healthy streak: {s:?}"
    );
}

#[test]
fn throttle_active_clamp_resets_the_healthy_streak_1116() {
    // A clamped pass must reset the consecutive-healthy streak to 0 (any re-onset breaks the run of
    // clean passes), so the hysteresis window restarts.
    let s = drive_state(
        "throttle_sig=\nthrottle_passes=0\nthrottle_clear_passes=5\n",
        &format!("BURST={}", shell_quote(CLAMPED_BURST)),
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_clear_passes"),
        "0",
        "an active clamp pass resets the consecutive-healthy streak: {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "imag-throttle:under-floor",
        "an active clamp sets the episode signature: {s:?}"
    );
}

#[test]
fn throttle_unmeasured_pass_preserves_streak_and_signature_1116() {
    // Regression guard for the issue-1076 contract under the new hysteresis: an UNMEASURED
    // (unknown) burst preserves BOTH the dedup signature and the consecutive-healthy streak — it
    // must not advance the streak toward clearing.
    let s = drive_state(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=4\nthrottle_clear_passes=3\n",
        "BURST=''",
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "imag-throttle:under-floor",
        "unmeasured -> preserve the dedup signature (issue 1076): {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_passes"),
        "4",
        "unmeasured -> preserve the pass count: {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_clear_passes"),
        "3",
        "unmeasured -> healthy streak unchanged (advances nothing): {s:?}"
    );
}

// =================================================================================================
// Part D — integration: alert_from_throttle render gate on the PAGE (notify recorder)
// =================================================================================================

#[test]
fn throttle_clamp_with_healthy_render_does_not_page_1116() {
    // THE RENDER-GATING BUG: a clamp while OBS render is HEALTHY must be log-only (no Discord page)
    // even on the first onset (prior_sig empty -> the throttle dedup would otherwise page now).
    let rec = drive_notify(
        "throttle_sig=\nthrottle_passes=0\n",
        &format!(
            "BURST={}\nRENDER={}",
            shell_quote(CLAMPED_BURST),
            shell_quote(HEALTHY_RENDER)
        ),
        "alert_from_throttle",
    );
    assert!(
        rec.is_empty(),
        "clamp + HEALTHY render must NOT fire a Discord page (log-only): recorder={rec:?}"
    );
}

#[test]
fn throttle_clamp_with_degraded_render_pages_1116() {
    // Control: a clamp WHILE render is degraded is the genuine actionable case -> it MUST page
    // (guards the gate does not over-suppress). alert_now=1 on the first onset (prior_sig empty).
    let rec = drive_notify(
        "throttle_sig=\nthrottle_passes=0\n",
        &format!(
            "BURST={}\nRENDER={}",
            shell_quote(CLAMPED_BURST),
            shell_quote(DEGRADED_RENDER)
        ),
        "alert_from_throttle",
    );
    assert!(
        rec.contains("notify"),
        "clamp + DEGRADED render must fire a Discord page: recorder={rec:?}"
    );
}

#[test]
fn throttle_clamp_with_unreadable_render_fails_open_and_pages_1116() {
    // Fail-open: an unreadable render (empty RENDER) during a real clamp must PAGE — never silently
    // suppress the alert on a WS read gap.
    let rec = drive_notify(
        "throttle_sig=\nthrottle_passes=0\n",
        &format!("BURST={}\nRENDER=''", shell_quote(CLAMPED_BURST)),
        "alert_from_throttle",
    );
    assert!(
        rec.contains("notify"),
        "clamp + UNREADABLE render must fail-open to a page (never silent): recorder={rec:?}"
    );
}

#[test]
fn throttle_clamp_with_healthy_render_still_advances_dedup_state_1116() {
    // "render provably healthy -> log-only report, dedup state STILL advances" — the log-only page
    // must not skip the dedup bookkeeping (so a later degraded pass is correctly throttled).
    let s = drive_state(
        "throttle_sig=\nthrottle_passes=0\n",
        &format!(
            "BURST={}\nRENDER={}",
            shell_quote(CLAMPED_BURST),
            shell_quote(HEALTHY_RENDER)
        ),
        "alert_from_throttle",
    );
    assert_eq!(
        field(&s, "throttle_sig"),
        "imag-throttle:under-floor",
        "log-only clamp still records the episode signature: {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_passes"),
        "1",
        "log-only clamp STILL advances the throttle dedup counter (0 -> 1): {s:?}"
    );
    assert_eq!(
        field(&s, "throttle_clear_passes"),
        "0",
        "an active (clamped) pass keeps the healthy streak at 0 even when log-only: {s:?}"
    );
}

// A render-healthy log-only clamp pass advances the throttle dedup counter (proven above). The
// documented, INTENTIONAL consequence: if render then DEGRADES mid-episode while the same clamp
// persists, the first degraded pass sees prior_sig==current_sig with the counter already part-way
// to the throttle window, so its page is deferred until the throttle re-arms (up to ~1h). This is
// accepted for a chronic condition whose real fix is cooling (issue 1043); it is BOUNDED, never
// permanently silent. These two tests pin BOTH halves of that contract.

#[test]
fn throttle_healthy_primed_then_degraded_defers_to_the_1h_throttle_1116() {
    // State as a render-healthy log-only pass would leave it (sig set, counter mid-window). A
    // render-DEGRADED pass now is throttle-suppressed (no page THIS pass) — the intentional ≤1h
    // first-page latency on a mid-episode healthy->degraded transition.
    let rec = drive_notify(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=1\n",
        &format!(
            "BURST={}\nRENDER={}",
            shell_quote(CLAMPED_BURST),
            shell_quote(DEGRADED_RENDER)
        ),
        "alert_from_throttle",
    );
    assert!(
        rec.is_empty(),
        "a degraded pass mid-throttle-window (primed by a prior log-only healthy pass) is \
         deferred, not paged this pass — the documented ≤1h latency: recorder={rec:?}"
    );
}

#[test]
fn throttle_healthy_primed_degraded_repages_after_the_throttle_window_1116() {
    // BOUNDED, never permanently silent: once the counter reaches ALERT_THROTTLE_PASSES (12), the
    // same persisting clamp-with-degraded-render re-arms and pages — the deferral above is at most
    // one throttle window (~1h), not a lost alert.
    let rec = drive_notify(
        "throttle_sig=imag-throttle:under-floor\nthrottle_passes=12\n",
        &format!(
            "BURST={}\nRENDER={}",
            shell_quote(CLAMPED_BURST),
            shell_quote(DEGRADED_RENDER)
        ),
        "alert_from_throttle",
    );
    assert!(
        rec.contains("notify"),
        "at the throttle-window edge the degraded clamp re-arms and PAGES (bounded, never silent): \
         recorder={rec:?}"
    );
}
