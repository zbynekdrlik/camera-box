//! #758 item 2 — `preflight_mv_reverify()`'s retry/settle budget, calibrated from a LIVE
//! measurement (2026-07-14), not guessed. The mechanism's first two real "Full-path E2E"
//! acceptance runs after this batch's push both failed cam1 with the ORIGINAL tight budget (a
//! single ~3.5s check, one re-attach, a fixed 2s settle, one more ~3.5s check — ~13s total after
//! the caller's own upfront `sleep 4`). A direct timed `systemctl restart camera-box` on cam1 +
//! repeated `frozen-camera-gate.py` polling against the SAME "MV NDI cam1" clone measured genuine
//! recovery at t+11.4s — inside the old budget's margin, but only barely, matching the two real
//! failures. A follow-up live repro (restart cam1, immediately call the ACTUAL new
//! `preflight_mv_reverify` body) confirmed recovery was only detected on the THIRD attempt
//! (~23s total) — i.e. the OLD ONE-re-attach budget would have failed this exact real scenario,
//! and the new multi-attempt budget correctly rides it out.
//!
//! Structural, source-text assertions (same discipline as the rest of this repo's harness suite)
//! since this is a read-only preflight probe against a live imag SSH host + live OBS-WS that
//! only the rig itself can exercise end-to-end (already proven live — see the commit message).

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn preflight_mv_reverify_retries_across_a_bounded_settle_loop_not_one_reattach() {
    let s = recording_e2e();
    assert!(
        s.contains("PREFLIGHT_MV_REVERIFY_ATTEMPTS")
            && s.contains("PREFLIGHT_MV_REVERIFY_SETTLE_S"),
        "#758: preflight_mv_reverify must expose its retry budget as overridable env vars, not \
         a hardcoded single re-attach + fixed sleep"
    );
    assert!(
        s.contains(r#"attempts="${PREFLIGHT_MV_REVERIFY_ATTEMPTS:-3}""#),
        "#758: default must be 3 attempts (live-calibrated: cam1's real recovery needed the \
         3rd attempt in a direct repro, so 3 is the minimum that actually rides out real jitter)"
    );
    assert!(
        s.contains(r#"settle_s="${PREFLIGHT_MV_REVERIFY_SETTLE_S:-6}""#),
        "#758: default settle must be 6s (comfortably above the 11.4s measured single-shot \
         recovery when spread across multiple attempts, matching the FROZEN_CAM_ATTEMPTS \
         precedent's 'attempts x settle' shape elsewhere in this file)"
    );
    assert!(
        s.contains("for a in $(seq 1 \"$attempts\")"),
        "#758: must be a real bounded loop over the configured attempt count"
    );
}

#[test]
fn preflight_mv_reverify_reattaches_only_once_on_the_first_failure() {
    let s = recording_e2e();
    let func_start = s
        .find("preflight_mv_reverify() {")
        .expect("preflight_mv_reverify must be defined");
    let func_end = s[func_start..]
        .find("\n}\n")
        .map(|i| func_start + i)
        .expect("function body must close with a bare }");
    let body = &s[func_start..func_end];
    let reattach_count = body.matches("strih_mv_scenes.py").count();
    assert_eq!(
        reattach_count, 1,
        "#758: re-attach must fire exactly ONCE (on the first failure), not on every retry — \
         repeated attempts are pure SETTLE time for the NDI reconnect the one re-attach already \
         triggered, got {reattach_count} strih_mv_scenes.py call(s) in the function body"
    );
    assert!(
        body.contains(r#"if [ "$a" -eq 1 ]; then"#),
        "#758: the re-attach must be gated to the FIRST attempt only"
    );
}

#[test]
fn preflight_mv_reverify_fail_message_reports_the_real_attempt_count_and_budget() {
    let s = recording_e2e();
    assert!(
        s.contains("still shows no pixel change after ${attempts} attempts"),
        "#758: the FAIL message must report the ACTUAL configured attempt count (not a stale \
         'ONE re-attach attempt' string left over from the old single-shot design)"
    );
    assert!(
        !s.contains("after ONE re-attach attempt"),
        "#758: the old single-attempt FAIL wording must be gone (replaced by the multi-attempt \
         budget's own message)"
    );
}
