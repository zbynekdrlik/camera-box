//! #1040 — pure-function guard for `scripts/lib/imag-power-envelope.sh`, the SHARED
//! power/thermal-envelope gather + verdict + guard-decision core for the imag notebook.
//!
//! Root cause (issues 799/880/1029/1030): the imag render regression is a HARDWARE power clamp —
//! the MMIO RAPL PL1 long-term constraint was programmed to 25 W (by thermald's DPTF policy),
//! starving the iGPU to `gt_act_freq` 600-850 MHz while every software knob sat at 1400. The
//! durable fix pins MMIO PL1 to a sustainable 29 W + `slpc_ignore_eff_freq=1` at boot, purges
//! thermald, and supervises the envelope with a loud root guard. This file pins the PURE core the
//! three surfaces (`setup-imag.sh` provisioning, `drift-guard.sh --check-imag`, `verify-imag.sh`)
//! all share, so the identity-based zone selection, the OK/DRIFT/UNKNOWN verdict, and the
//! step-down/restore/re-assert guard decision are correct regardless of any live box.
//!
//! Same convention as `tests/verify_imag_pure_functions.rs` / `tests/drift_guard.rs`: source the
//! REAL lib (it is source-only, no side effects) and exercise the pure functions directly.
//!
//! RED before `scripts/lib/imag-power-envelope.sh` exists (sourcing fails, every test fails);
//! GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/imag-power-envelope.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib (plus `timesync-authority.sh`, whose generic `dpkg_status_installed` /
/// `timesync_enabled_state_neutral` the thermald verdict reuses) and run `body` against its pure
/// functions. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!(
        "set -uo pipefail\n. \"$TSLIB\"\n. \"$LIB\"\n{body}",
        body = body
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .env(
            "TSLIB",
            manifest_dir().join("scripts/lib/timesync-authority.sh"),
        )
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// A CLEAN gather block: package-0 long_term = 29 W, enabled, both slpc knobs = 1, thermald PURGED
// (empty dpkg status + inactive + not-found), both units enabled+active, TCPU well below the
// ceiling. Deliberately puts a NON-package zone FIRST and long_term at a NON-zero constraint index
// so identity-based selection is genuinely exercised (never a hardcoded `:0`/index 0).
const CLEAN_GATHER: &str = "\
ZONE|core
CONSTRAINT|core|0|long_term|15000000
ENABLED|core|1
ZONE|package-0
CONSTRAINT|package-0|0|peak_power|51000000
CONSTRAINT|package-0|1|long_term|29000000
CONSTRAINT|package-0|2|short_term|51000000
ENABLED|package-0|1
SLPC|1
SLPC|1
THERMALD||inactive|not-found
UNIT|imag-power-envelope.service|enabled|active
UNIT|imag-power-envelope-guard.timer|enabled|active
TCPU|84
ACTFREQ|1350
PL2_UW|51000000
";

// ---------------------------------------------------------------------------------------------
// lib shape
// ---------------------------------------------------------------------------------------------

#[test]
fn lib_exists_is_source_only_and_reuses_the_generic_dpkg_helper() {
    let body = std::fs::read_to_string(lib()).unwrap();
    assert!(
        body.starts_with("#!/usr/bin/env bash") || body.starts_with("#!/bin/bash"),
        "lib must be a bash script"
    );
    // Source-only means it must not ENABLE errexit as a STATEMENT — a bare `body.contains("set -e")`
    // is too broad (it also matches the header prose "must NOT impose `set -euo pipefail`"), so
    // check for an actual errexit-enabling line instead.
    let enables_errexit = body.lines().any(|l| {
        let t = l.trim();
        !t.starts_with('#')
            && (t.starts_with("set -e") || t.starts_with("set -euo") || t == "set -o errexit")
    });
    assert!(
        !enables_errexit,
        "the shared lib must be SOURCE-ONLY (never enable errexit) — mirrors scripts/lib/timesync-authority.sh"
    );
    // Reuse, never re-implement, the generic package/enabled-state helpers.
    assert!(
        body.contains("dpkg_status_installed"),
        "thermald verdict must reuse the generic dpkg_status_installed (timesync-authority.sh)"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_pl1_watts_to_uw / imag_pl1_uw_matches_pin
// ---------------------------------------------------------------------------------------------

#[test]
fn pl1_watts_to_microwatts_conversion_is_exact() {
    let (_c, out, _e) = run_sourced("imag_pl1_watts_to_uw 29; imag_pl1_watts_to_uw 25");
    assert_eq!(
        out.trim(),
        "29000000\n25000000".trim(),
        "29 W -> 29000000 uW, 25 W -> 25000000 uW (exact integer): {out:?}"
    );
}

#[test]
fn pl1_uw_matches_pin_true_only_on_exact_watt_equivalence() {
    let (c_ok, _o, _e) =
        run_sourced("imag_pl1_uw_matches_pin 29000000 29 && echo Y || echo N");
    assert!(c_ok == 0);
    let (_c, out, _e) = run_sourced(
        "imag_pl1_uw_matches_pin 29000000 29 && echo Y || echo N\n\
         imag_pl1_uw_matches_pin 25000000 29 && echo Y || echo N\n\
         imag_pl1_uw_matches_pin '' 29 && echo Y || echo N",
    );
    assert_eq!(
        out.trim(),
        "Y\nN\nN",
        "29e6 matches 29 W; 25e6 does not match 29 W; empty never matches: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_power_zone_select — identity-based, never a hardcoded index
// ---------------------------------------------------------------------------------------------

#[test]
fn pl1_zone_selected_by_package_name_never_a_hardcoded_index() {
    // package-0's long_term sits at constraint index 1, AND a `core` zone precedes it whose own
    // index-0 constraint is also `long_term` (15 W). A hardcoded `:0`/first-constraint selection
    // would return 15000000; identity-based selection must return package-0's long_term 29000000.
    let (_c2, out2, _e2) = run_sourced_with_gather("imag_power_zone_select \"$G\"", CLEAN_GATHER);
    assert_eq!(
        out2.trim(),
        "29000000",
        "must select the package-0 long_term constraint by NAME identity, not index 0 (which is \
         the peak_power 51 W here) nor the preceding `core` zone's long_term (15 W): {out2:?}"
    );
}

#[test]
fn pl1_zone_select_empty_when_no_package_zone() {
    let (_c, out, _e) =
        run_sourced_with_gather("imag_power_zone_select \"$G\" || echo MISSING", "ZONE|core\nCONSTRAINT|core|0|long_term|15000000\n");
    assert!(
        out.contains("MISSING") && !out.contains("15000000"),
        "no package-0 zone -> empty + nonzero, never the wrong zone's value: {out:?}"
    );
}

// helper that injects the gather via env G
fn run_sourced_with_gather(body: &str, gather: &str) -> (i32, String, String) {
    let harness = format!(
        "set -uo pipefail\n. \"$TSLIB\"\n. \"$LIB\"\nG=$(cat <<'__G__'\n{gather}__G__\n)\n{body}",
        gather = gather,
        body = body
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .env(
            "TSLIB",
            manifest_dir().join("scripts/lib/timesync-authority.sh"),
        )
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// imag_power_envelope_verdict — per-facet OK / DRIFT / UNKNOWN
// ---------------------------------------------------------------------------------------------

fn verdict(gather: &str, pinned_watts: &str) -> String {
    let (_c, out, _e) = run_sourced_with_gather(
        &format!("imag_power_envelope_verdict \"$G\" {pinned_watts}"),
        gather,
    );
    out
}

fn facet_status(out: &str, facet: &str) -> String {
    out.lines()
        .find(|l| l.starts_with(&format!("{facet}|")))
        .unwrap_or_else(|| panic!("no {facet} verdict line in: {out:?}"))
        .split('|')
        .nth(1)
        .unwrap_or("")
        .to_string()
}

#[test]
fn verdict_ok_when_pl1_matches_slpc_one_thermald_purged_units_up() {
    let out = verdict(CLEAN_GATHER, "29");
    for f in ["pl1", "slpc", "thermald", "units"] {
        assert_eq!(facet_status(&out, f), "OK", "facet {f} must be OK on the clean gather: {out:?}");
    }
}

#[test]
fn verdict_unknown_when_gather_block_is_empty_never_a_false_drift() {
    let out = verdict("", "29");
    for f in ["pl1", "slpc", "thermald", "units"] {
        assert_eq!(
            facet_status(&out, f),
            "UNKNOWN",
            "empty gather -> {f} UNKNOWN (an SSH hiccup), NEVER a false DRIFT: {out:?}"
        );
    }
    assert!(!out.contains("DRIFT"), "no DRIFT on an empty gather: {out:?}");
}

#[test]
fn verdict_drift_when_pl1_differs_from_the_pinned_watts() {
    // Live 25 W clamp, pin 29 W -> pl1 DRIFT (this IS the whole regression signature).
    let g = CLEAN_GATHER.replace("long_term|29000000", "long_term|25000000");
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "pl1"), "DRIFT", "25 W vs pinned 29 W is DRIFT: {out:?}");
}

#[test]
fn verdict_drift_when_pl1_enabled_is_zero() {
    // Correct watts but the constraint is DISABLED -> the limit is not actually enforced -> DRIFT.
    let g = CLEAN_GATHER.replace("ENABLED|package-0|1", "ENABLED|package-0|0");
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "pl1"), "DRIFT", "PL1 enabled=0 is DRIFT even at the right watts: {out:?}");
}

#[test]
fn verdict_unknown_when_no_pin_supplied_never_a_false_drift() {
    let out = verdict(CLEAN_GATHER, "\"\"");
    assert_eq!(facet_status(&out, "pl1"), "UNKNOWN", "no pinned watts -> pl1 UNKNOWN: {out:?}");
}

#[test]
fn verdict_drift_when_any_slpc_knob_reads_zero() {
    let g = CLEAN_GATHER.replacen("SLPC|1\nSLPC|1", "SLPC|1\nSLPC|0", 1);
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "slpc"), "DRIFT", "any slpc knob at 0 is DRIFT: {out:?}");
}

#[test]
fn verdict_unknown_slpc_when_no_knob_discovered() {
    let g = CLEAN_GATHER.replace("SLPC|1\nSLPC|1\n", "");
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "slpc"), "UNKNOWN", "no slpc knob discovered -> UNKNOWN: {out:?}");
}

#[test]
fn thermald_installed_even_masked_is_a_fail() {
    // The whole point of PURGE-not-mask: an INSTALLED thermald (even masked+inactive) is DRIFT.
    let g = CLEAN_GATHER.replace(
        "THERMALD||inactive|not-found",
        "THERMALD|install ok installed|inactive|masked",
    );
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "thermald"),
        "DRIFT",
        "thermald installed (even masked+inactive) must be DRIFT — masking is not enough: {out:?}"
    );
}

#[test]
fn thermald_active_is_a_fail() {
    let g = CLEAN_GATHER.replace(
        "THERMALD||inactive|not-found",
        "THERMALD||active|not-found",
    );
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "thermald"), "DRIFT", "thermald active is DRIFT: {out:?}");
}

#[test]
fn units_drift_when_the_guard_timer_is_dead() {
    // A correct PL1 with a DEAD guard is the "provisioned but unsupervised" shape — DRIFT.
    let g = CLEAN_GATHER.replace(
        "UNIT|imag-power-envelope-guard.timer|enabled|active",
        "UNIT|imag-power-envelope-guard.timer|enabled|inactive",
    );
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "units"), "DRIFT", "a dead guard timer is DRIFT: {out:?}");
}

#[test]
fn units_unknown_when_states_not_gathered() {
    let g = CLEAN_GATHER
        .replace("UNIT|imag-power-envelope.service|enabled|active\n", "")
        .replace("UNIT|imag-power-envelope-guard.timer|enabled|active\n", "");
    let out = verdict(&g, "29");
    assert_eq!(facet_status(&out, "units"), "UNKNOWN", "no unit rows -> UNKNOWN: {out:?}");
}

// ---------------------------------------------------------------------------------------------
// imag_power_guard_decision — stepdown | restore | reassert | hold
// signature: CURRENT_UW EXPECTED_UW STEPDOWN_UW TCPU_C CEIL_C RESTORE_C HOT_STREAK COOL_STREAK STEPPED_DOWN
// ---------------------------------------------------------------------------------------------

fn decision(args: &str) -> String {
    let (_c, out, _e) = run_sourced(&format!("imag_power_guard_decision {args}"));
    out.trim().to_string()
}

#[test]
fn guard_steps_down_at_the_tcpu_ceiling_only_after_two_consecutive_hot_reads() {
    // At the ceiling with NO prior hot read (HOT_STREAK=0) -> hold (wait for the 2nd).
    assert_eq!(
        decision("29000000 29000000 25000000 94 93 85 0 0 0"),
        "hold",
        "first hot read at the ceiling must HOLD, not step down (needs 2 consecutive)"
    );
    // At the ceiling WITH one prior hot read (HOT_STREAK=1) -> this is the 2nd -> stepdown.
    assert_eq!(
        decision("29000000 29000000 25000000 94 93 85 1 0 0"),
        "stepdown",
        "the 2nd consecutive hot read at the ceiling must STEP DOWN"
    );
    // Already stepped down: never step down again (idempotent) -> hold.
    assert_eq!(
        decision("25000000 29000000 25000000 94 93 85 3 0 1"),
        "hold",
        "already stepped down -> hold (do not re-step)"
    );
}

#[test]
fn guard_restores_only_after_sustained_recovery() {
    // Stepped down, cooled below the restore threshold but NOT yet sustained (COOL_STREAK=0) -> hold.
    assert_eq!(
        decision("25000000 29000000 25000000 80 93 85 0 0 1"),
        "hold",
        "first cool read must HOLD, not restore (recovery must be sustained)"
    );
    // Stepped down, cooled AND sustained (COOL_STREAK=1) -> restore.
    assert_eq!(
        decision("25000000 29000000 25000000 80 93 85 0 1 1"),
        "restore",
        "sustained recovery must RESTORE the full envelope"
    );
    // Cooled but still ABOVE the restore threshold -> hold (hysteresis band).
    assert_eq!(
        decision("25000000 29000000 25000000 88 93 85 0 5 1"),
        "hold",
        "between restore (85) and ceiling (93) is the hysteresis band -> hold"
    );
}

#[test]
fn guard_reasserts_a_foreign_pl1_write() {
    // Not stepped down, temp nominal, but the live PL1 no longer equals the expected envelope
    // (something re-programmed it) -> re-assert.
    assert_eq!(
        decision("25000000 29000000 25000000 70 93 85 0 0 0"),
        "reassert",
        "a foreign PL1 write while at nominal temp must be RE-ASSERTED"
    );
}

#[test]
fn guard_holds_when_nominal_and_envelope_intact() {
    assert_eq!(
        decision("29000000 29000000 25000000 70 93 85 0 0 0"),
        "hold",
        "nominal temp + envelope intact -> hold (no action)"
    );
}

#[test]
fn guard_holds_when_temperature_unreadable_never_a_blind_step() {
    // Empty TCPU (sensor unreadable) must never trigger a thermal step-down or restore.
    assert_eq!(
        decision("29000000 29000000 25000000 '' 93 85 5 5 0"),
        "hold",
        "an unreadable TCPU must HOLD — never a blind step-down"
    );
}

// ---------------------------------------------------------------------------------------------
// gather remote snippet — identity-based selection baked into the emitted shell
// ---------------------------------------------------------------------------------------------

#[test]
fn gather_remote_snippet_selects_by_identity_and_globs_all_cards() {
    let (_c, out, _e) = run_sourced("imag_power_envelope_gather_remote_snippet");
    // RAPL: the gather iterates the mmio zones and emits each constraint's NAME field so the
    // verdict (imag_power_zone_select) can select package-0/long_term by IDENTITY downstream — the
    // literal `package-0`/`long_term` values are RUNTIME, not in the snippet text (that identity
    // selection is covered by pl1_zone_selected_by_package_name_never_a_hardcoded_index).
    assert!(
        out.contains("intel-rapl-mmio:") && out.contains("constraint_") && out.contains("_name"),
        "the gather must iterate intel-rapl-mmio:* and emit each constraint's NAME field: {out:?}"
    );
    // slpc: glob across ALL drm cards (the presenter-drm cardN renumbering hazard).
    assert!(
        out.contains("slpc_ignore_eff_freq") && out.contains("card*"),
        "the gather must glob slpc across card* (never a hardcoded cardN): {out:?}"
    );
    // thermald + the two units must be gathered too.
    assert!(
        out.contains("thermald")
            && out.contains("imag-power-envelope.service")
            && out.contains("imag-power-envelope-guard.timer"),
        "the gather must collect thermald + both envelope units: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// README pin-present guard — the drift-guard authority for the strict PL1 gate
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// imag_power_alert_condition — the dev1-side watchdog pages on STEP-DOWN / RE-ASSERT, never RESTORE
// ---------------------------------------------------------------------------------------------

#[test]
fn alert_condition_fires_on_stepdown_and_reassert_but_not_restore_or_hold() {
    // A journal window with a STEP-DOWN line -> alertable.
    let (_c, out, _e) = run_sourced(
        "imag_power_alert_condition 'Aug 13 imag-nb imag-power-envelope[1]: STEP-DOWN: TCPU=94C ...'",
    );
    assert!(out.contains("STEP-DOWN"), "STEP-DOWN must be an alert condition: {out:?}");

    // A RE-ASSERT line -> alertable.
    let (_c, out2, _e) = run_sourced(
        "imag_power_alert_condition 'Aug 13 imag-nb imag-power-envelope[1]: RE-ASSERT: live PL1=... foreign ...'",
    );
    assert!(out2.contains("RE-ASSERT"), "RE-ASSERT must be an alert condition: {out2:?}");

    // A RESTORE-only window (recovery) -> NOT alertable (informational).
    let (_c, out3, _e) = run_sourced(
        "imag_power_alert_condition 'Aug 13 imag-nb imag-power-envelope[1]: RESTORE: TCPU=80C sustained ...'",
    );
    assert!(
        out3.trim().is_empty(),
        "RESTORE (recovery) must NOT page — only degradations do: {out3:?}"
    );

    // An empty window -> nothing.
    let (_c, out4, _e) = run_sourced("imag_power_alert_condition ''");
    assert!(out4.trim().is_empty(), "no markers -> no alert: {out4:?}");
}

// ---------------------------------------------------------------------------------------------
// on-box scripts — the oneshot (imag-power-envelope.sh) + the guard (imag-power-envelope-guard.sh)
// ---------------------------------------------------------------------------------------------

fn read_script(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn oneshot_is_a_failloud_script_that_sets_the_envelope_by_identity() {
    let body = read_script("scripts/imag-power-envelope.sh");
    assert!(body.contains("set -euo pipefail"), "the oneshot must fail loud");
    // slpc across ALL cards, PL1 by package-0/long_term NAME identity (never a hardcoded index).
    assert!(
        body.contains("slpc_ignore_eff_freq") && body.contains("card*"),
        "the oneshot must set slpc across every card* (never a hardcoded cardN)"
    );
    assert!(
        body.contains("intel-rapl-mmio:")
            && body.contains("package-0")
            && body.contains("long_term"),
        "the oneshot must select the RAPL zone/constraint by package-0/long_term NAME identity"
    );
    // Hardware-agnostic (816): no zone -> exit 0; a box that HAS the zone must FATAL if the write
    // did not take (a silently-unapplied envelope is the exact regression this closes).
    assert!(
        body.contains("hardware-agnostic") && body.contains("did not take"),
        "the oneshot must be hardware-agnostic yet assert the write took on a box that has the zone"
    );
}

#[test]
fn guard_uses_the_shared_decision_never_a_second_copy() {
    let body = read_script("scripts/imag-power-envelope-guard.sh");
    assert!(body.contains("set -euo pipefail"), "the guard must fail loud");
    // The DECISION must be the shared pure function, not re-implemented inline.
    assert!(
        body.contains("imag_power_guard_decision"),
        "the guard must call the shared imag_power_guard_decision (one source of truth)"
    );
    // It must source the shared lib (installed path + repo fallback).
    assert!(
        body.contains("/usr/local/lib/imag-power-envelope.sh"),
        "the guard must source the installed shared lib"
    );
    // Every transition is journald-tagged so a clamp episode is retrievable + alertable.
    assert!(
        body.contains("logger -t") && body.contains("STEP-DOWN") && body.contains("RE-ASSERT"),
        "the guard must journald-tag its step-down / re-assert transitions (dev1-side alerting)"
    );
}

#[test]
fn dev1_alert_watchdog_reuses_the_shared_condition_and_throttle() {
    let body = read_script("scripts/imag-power-envelope-alert-watchdog.sh");
    assert!(body.contains("set -uo pipefail"), "a watchdog must survive per-pass failures (not set -e)");
    // Reuses the SHARED alert-condition + the SHARED throttle — no second alert mechanism.
    assert!(
        body.contains("imag_power_alert_condition"),
        "the watchdog must decide via the shared imag_power_alert_condition"
    );
    assert!(
        body.contains("obs_watchdog_alert_throttle"),
        "the watchdog must reuse the shared alert throttle (#391/#882), not a second one"
    );
    // Fires the alert from dev1 (imag-nb has no airuleset checkout / Discord creds).
    assert!(
        body.contains("airuleset.py") || body.contains("$NOTIFY"),
        "the watchdog fires airuleset.py notify from dev1"
    );
}

#[test]
fn dev1_alert_watchdog_dry_run_never_fires() {
    // --dry-run must measure+decide+log only and exit 0 without any airuleset call. With no reachable
    // imag-nb the measure step yields an empty journal -> "nothing to decide" -> clean exit 0.
    let p = manifest_dir().join("scripts/imag-power-envelope-alert-watchdog.sh");
    let out = Command::new("bash")
        .arg(&p)
        .arg("--dry-run")
        .env("IMAG_IP", "203.0.113.1") // TEST-NET-3, unreachable -> empty measure
        .env("IMAG_POWER_ALERT_STATE_DIR", std::env::temp_dir())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run watchdog --dry-run");
    assert!(
        out.status.success(),
        "watchdog --dry-run must exit 0 even with no reachable box: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn readme_pins_power_pl1_w_imag_at_29() {
    let readme = std::fs::read_to_string(manifest_dir().join("vendor/README.md")).unwrap();
    let has = readme
        .lines()
        .any(|l| l.contains("power_pl1_w_imag") && l.contains("`29`"));
    assert!(
        has,
        "vendor/README.md must pin `power_pl1_w_imag` = `29` — without the pin the drift-guard \
         PL1 facet reads UNKNOWN forever and the strict gate is inert"
    );
}
