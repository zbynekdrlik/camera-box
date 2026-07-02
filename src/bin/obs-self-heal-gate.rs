//! #411 — obs-self-heal-gate CLI (default features, no probe deps).
//!
//! The Windows-local recovery script (`scripts/obs-self-heal-install.sh`'s
//! `build_recovery_script`) needs the SAME confirm-threshold / min-interval-throttle /
//! single-recovery-lock decision `camera_box::obs_self_heal::decide` makes — but PowerShell
//! cannot call Rust directly. Mirroring how `obs-watchdog-gate` bridges the wedge VERDICT
//! (`obs_watchdog::classify`) into the recovery script, this binary bridges the RECOVERY
//! decision the same way, so the confirm/throttle/lock policy is decided in exactly ONE
//! place — never re-derived in PowerShell.
//!
//! Reads a JSON object from stdin:
//!
//! ```json
//! {
//!   "confirm_count": 0,
//!   "last_attempt_epoch_s": null,
//!   "recovery_in_progress": false,
//!   "wedged": true,
//!   "now_epoch_s": 1751500000,
//!   "threshold": null,
//!   "min_interval_s": null,
//!   "stale_lock_s": null
//! }
//! ```
//!
//! `confirm_count`, `recovery_in_progress`, `wedged`, and `now_epoch_s` are REQUIRED (the
//! caller always knows its own persisted state and this pass's verdict + clock — a missing
//! required field is a wiring bug, not a value to guess). `last_attempt_epoch_s` is optional
//! (`null` = never attempted). `threshold` / `min_interval_s` / `stale_lock_s` are OPTIONAL —
//! omit or pass `null` to use `camera_box::obs_self_heal`'s own `DEFAULT_*` constants, which
//! makes those constants the single actual source of truth for a default install (never a
//! second hardcoded literal in the install script that could silently drift).
//!
//! `lock_is_stale` + `clear_recovery_lock` are applied BEFORE `decide` (the documented
//! caller-composition pattern from `obs_self_heal.rs`), so a crashed prior recovery cannot
//! permanently wedge the state machine.
//!
//! Prints a JSON object to stdout:
//!
//! ```json
//! {
//!   "decision": "Healthy" | "Confirming" | "Throttled" | "AlreadyRecovering" | "Recover",
//!   "confirm_count": 1,
//!   "threshold": 2,
//!   "seconds_remaining": 570,
//!   "steps": ["StopAhk", "KillAndRelaunchObs", "VerifyRecovered", "RestartAhk"],
//!   "next_state": {
//!     "confirm_count": 2,
//!     "last_attempt_epoch_s": 1751500000,
//!     "recovery_in_progress": true
//!   }
//! }
//! ```
//!
//! `next_state` is ALWAYS present — the caller persists it verbatim, every pass, regardless
//! of decision (even `Healthy` resets `confirm_count`, which must be saved). `confirm_count`
//! (top-level) and `threshold` are populated for `Confirming`; `seconds_remaining` for
//! `Throttled`; `steps` for `Recover` (empty array otherwise).
//!
//! **Exit codes** (mirrors `obs-watchdog-gate`'s 0/1/2 contract exactly):
//! - `0` — no action needed this pass (`Healthy` / `Confirming` / `Throttled` /
//!   `AlreadyRecovering`)
//! - `1` — ACT NOW (`Recover`) — the caller runs the 4-step recovery plan
//! - `2` — bad JSON / missing required field → prints `ERROR: <reason>` to stderr

use camera_box::obs_self_heal::{
    clear_recovery_lock, decide, lock_is_stale, RecoveryStep, SelfHealDecision, SelfHealState,
    DEFAULT_CONFIRM_THRESHOLD, DEFAULT_MIN_RECOVERY_INTERVAL_S, DEFAULT_STALE_LOCK_S,
};
use std::io::Read;

fn req_bool(v: &serde_json::Value, key: &str) -> Result<bool, String> {
    v.get(key)
        .and_then(|x| x.as_bool())
        .ok_or_else(|| format!("missing/invalid required bool field '{key}'"))
}

fn req_u64(v: &serde_json::Value, key: &str) -> Result<u64, String> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| format!("missing/invalid required non-negative integer field '{key}'"))
}

fn req_u32(v: &serde_json::Value, key: &str) -> Result<u32, String> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("missing/invalid required non-negative integer field '{key}'"))
}

fn opt_u64(v: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("field '{key}' is not a non-negative integer")),
    }
}

fn opt_u32(v: &serde_json::Value, key: &str) -> Result<Option<u32>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("field '{key}' is not a non-negative integer")),
    }
}

fn step_name(s: &RecoveryStep) -> &'static str {
    match s {
        RecoveryStep::StopAhk => "StopAhk",
        RecoveryStep::KillAndRelaunchObs => "KillAndRelaunchObs",
        RecoveryStep::VerifyRecovered => "VerifyRecovered",
        RecoveryStep::RestartAhk => "RestartAhk",
    }
}

fn state_to_json(s: &SelfHealState) -> serde_json::Value {
    serde_json::json!({
        "confirm_count": s.confirm_count,
        "last_attempt_epoch_s": s.last_attempt_epoch_s,
        "recovery_in_progress": s.recovery_in_progress,
    })
}

fn run(input: &str) -> Result<(serde_json::Value, i32), String> {
    let raw: serde_json::Value =
        serde_json::from_str(input).map_err(|e| format!("JSON parse: {e}"))?;

    let confirm_count = req_u32(&raw, "confirm_count")?;
    let recovery_in_progress = req_bool(&raw, "recovery_in_progress")?;
    let wedged = req_bool(&raw, "wedged")?;
    let now_epoch_s = req_u64(&raw, "now_epoch_s")?;
    let last_attempt_epoch_s = opt_u64(&raw, "last_attempt_epoch_s")?;
    let threshold = opt_u32(&raw, "threshold")?.unwrap_or(DEFAULT_CONFIRM_THRESHOLD);
    let min_interval_s =
        opt_u64(&raw, "min_interval_s")?.unwrap_or(DEFAULT_MIN_RECOVERY_INTERVAL_S);
    let stale_lock_s = opt_u64(&raw, "stale_lock_s")?.unwrap_or(DEFAULT_STALE_LOCK_S);

    let mut state = SelfHealState {
        confirm_count,
        last_attempt_epoch_s,
        recovery_in_progress,
    };

    // Caller-composition pattern documented in obs_self_heal.rs: a stale lock (the process
    // that set it almost certainly crashed) is cleared BEFORE deciding, so one abandoned run
    // never permanently disables self-heal for the box.
    if lock_is_stale(&state, now_epoch_s, stale_lock_s) {
        state = clear_recovery_lock(state);
    }

    let (decision, next_state) = decide(state, wedged, now_epoch_s, threshold, min_interval_s);

    let (decision_name, extra, exit_code) = match &decision {
        SelfHealDecision::Healthy => ("Healthy", serde_json::json!({}), 0),
        SelfHealDecision::Confirming { count, threshold } => (
            "Confirming",
            serde_json::json!({"confirm_count": count, "threshold": threshold}),
            0,
        ),
        SelfHealDecision::Throttled { seconds_remaining } => (
            "Throttled",
            serde_json::json!({"seconds_remaining": seconds_remaining}),
            0,
        ),
        SelfHealDecision::AlreadyRecovering => ("AlreadyRecovering", serde_json::json!({}), 0),
        SelfHealDecision::Recover(steps) => (
            "Recover",
            serde_json::json!({"steps": steps.iter().map(step_name).collect::<Vec<_>>()}),
            1,
        ),
    };

    let mut out = serde_json::json!({
        "decision": decision_name,
        "next_state": state_to_json(&next_state),
    });
    if let serde_json::Value::Object(ref mut map) = out {
        if let serde_json::Value::Object(extra_map) = extra {
            map.extend(extra_map);
        }
    }

    Ok((out, exit_code))
}

fn main() {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("ERROR: reading stdin: {e}");
        std::process::exit(2);
    }

    match run(&input) {
        Ok((out, exit_code)) => {
            println!("{out}");
            std::process::exit(exit_code);
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_pass_exits_zero_and_resets_confirm_count() {
        let input = r#"{"confirm_count":3,"last_attempt_epoch_s":null,
            "recovery_in_progress":false,"wedged":false,"now_epoch_s":1000}"#;
        let (out, code) = run(input).expect("should parse");
        assert_eq!(code, 0);
        assert_eq!(out["decision"], "Healthy");
        assert_eq!(out["next_state"]["confirm_count"], 0);
    }

    #[test]
    fn confirmed_wedge_exits_one_with_the_canonical_step_plan() {
        let input = r#"{"confirm_count":1,"last_attempt_epoch_s":null,
            "recovery_in_progress":false,"wedged":true,"now_epoch_s":1000,
            "threshold":2,"min_interval_s":600}"#;
        let (out, code) = run(input).expect("should parse");
        assert_eq!(code, 1, "a confirmed wedge must exit 1 (ACT)");
        assert_eq!(out["decision"], "Recover");
        assert_eq!(
            out["steps"],
            serde_json::json!([
                "StopAhk",
                "KillAndRelaunchObs",
                "VerifyRecovered",
                "RestartAhk"
            ])
        );
        assert_eq!(out["next_state"]["recovery_in_progress"], true);
    }

    #[test]
    fn omitted_threshold_and_interval_default_to_the_rust_kernel_constants() {
        // No "threshold"/"min_interval_s"/"stale_lock_s" fields at all — DEFAULT_* must apply.
        // DEFAULT_CONFIRM_THRESHOLD=2: a PRIOR confirm_count of 0 means this pass's wedged
        // reading becomes the FIRST consecutive one (incremented to 1 inside decide()) — not
        // yet at the threshold, so it must only Confirm, never act.
        let input = r#"{"confirm_count":0,"last_attempt_epoch_s":null,
            "recovery_in_progress":false,"wedged":true,"now_epoch_s":1000}"#;
        let (out, code) = run(input).expect("should parse");
        assert_eq!(code, 0);
        assert_eq!(out["decision"], "Confirming");
        assert_eq!(out["threshold"], DEFAULT_CONFIRM_THRESHOLD);
    }

    #[test]
    fn held_lock_always_wins_regardless_of_wedged() {
        let input = r#"{"confirm_count":5,"last_attempt_epoch_s":900,
            "recovery_in_progress":true,"wedged":true,"now_epoch_s":1000,
            "stale_lock_s":900000}"#;
        let (out, code) = run(input).expect("should parse");
        assert_eq!(code, 0);
        assert_eq!(out["decision"], "AlreadyRecovering");
    }

    #[test]
    fn a_stale_lock_is_cleared_before_deciding_so_a_confirmed_wedge_can_still_recover() {
        // Lock held since epoch 0, now is 10_000s later, stale_lock_s=900 -> stale -> cleared
        // -> a wedged pass with confirm_count already at threshold can Recover again.
        let input = r#"{"confirm_count":2,"last_attempt_epoch_s":0,
            "recovery_in_progress":true,"wedged":true,"now_epoch_s":10000,
            "threshold":2,"min_interval_s":0,"stale_lock_s":900}"#;
        let (out, code) = run(input).expect("should parse");
        assert_eq!(code, 1, "a stale lock must be clearable, not stuck forever");
        assert_eq!(out["decision"], "Recover");
    }

    #[test]
    fn missing_required_field_is_a_parse_error_exit_two() {
        let err = run(r#"{"wedged":true}"#).unwrap_err();
        assert!(err.contains("confirm_count"));
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = run("not json").unwrap_err();
        assert!(err.contains("JSON parse"));
    }
}
