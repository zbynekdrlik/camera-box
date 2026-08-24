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

/// Wrap an arbitrary string as a single bash-safe single-quoted argument.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    let (c_ok, _o, _e) = run_sourced("imag_pl1_uw_matches_pin 29000000 29 && echo Y || echo N");
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
    let (_c, out, _e) = run_sourced_with_gather(
        "imag_power_zone_select \"$G\" || echo MISSING",
        "ZONE|core\nCONSTRAINT|core|0|long_term|15000000\n",
    );
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
        assert_eq!(
            facet_status(&out, f),
            "OK",
            "facet {f} must be OK on the clean gather: {out:?}"
        );
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
    assert!(
        !out.contains("DRIFT"),
        "no DRIFT on an empty gather: {out:?}"
    );
}

#[test]
fn verdict_drift_when_pl1_differs_from_the_pinned_watts() {
    // Live 25 W clamp, pin 29 W -> pl1 DRIFT (this IS the whole regression signature).
    let g = CLEAN_GATHER.replace("long_term|29000000", "long_term|25000000");
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "pl1"),
        "DRIFT",
        "25 W vs pinned 29 W is DRIFT: {out:?}"
    );
}

#[test]
fn verdict_drift_when_pl1_enabled_is_zero() {
    // Correct watts but the constraint is DISABLED -> the limit is not actually enforced -> DRIFT.
    let g = CLEAN_GATHER.replace("ENABLED|package-0|1", "ENABLED|package-0|0");
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "pl1"),
        "DRIFT",
        "PL1 enabled=0 is DRIFT even at the right watts: {out:?}"
    );
}

#[test]
fn verdict_unknown_when_no_pin_supplied_never_a_false_drift() {
    let out = verdict(CLEAN_GATHER, "\"\"");
    assert_eq!(
        facet_status(&out, "pl1"),
        "UNKNOWN",
        "no pinned watts -> pl1 UNKNOWN: {out:?}"
    );
}

#[test]
fn verdict_drift_when_any_slpc_knob_reads_zero() {
    let g = CLEAN_GATHER.replacen("SLPC|1\nSLPC|1", "SLPC|1\nSLPC|0", 1);
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "slpc"),
        "DRIFT",
        "any slpc knob at 0 is DRIFT: {out:?}"
    );
}

#[test]
fn verdict_unknown_slpc_when_no_knob_discovered() {
    let g = CLEAN_GATHER.replace("SLPC|1\nSLPC|1\n", "");
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "slpc"),
        "UNKNOWN",
        "no slpc knob discovered -> UNKNOWN: {out:?}"
    );
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
    let g = CLEAN_GATHER.replace("THERMALD||inactive|not-found", "THERMALD||active|not-found");
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "thermald"),
        "DRIFT",
        "thermald active is DRIFT: {out:?}"
    );
}

#[test]
fn units_drift_when_the_guard_timer_is_dead() {
    // A correct PL1 with a DEAD guard is the "provisioned but unsupervised" shape — DRIFT.
    let g = CLEAN_GATHER.replace(
        "UNIT|imag-power-envelope-guard.timer|enabled|active",
        "UNIT|imag-power-envelope-guard.timer|enabled|inactive",
    );
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "units"),
        "DRIFT",
        "a dead guard timer is DRIFT: {out:?}"
    );
}

#[test]
fn units_unknown_when_states_not_gathered() {
    let g = CLEAN_GATHER
        .replace("UNIT|imag-power-envelope.service|enabled|active\n", "")
        .replace("UNIT|imag-power-envelope-guard.timer|enabled|active\n", "");
    let out = verdict(&g, "29");
    assert_eq!(
        facet_status(&out, "units"),
        "UNKNOWN",
        "no unit rows -> UNKNOWN: {out:?}"
    );
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
// imag_power_pl1_pin_from_readme_text — verify-imag reads the SAME authority drift-guard reads
// ---------------------------------------------------------------------------------------------

#[test]
fn pin_reader_extracts_power_pl1_w_imag_from_the_readme_row() {
    let readme = "| setting | pinned value | live source |\n\
                  |---|---|---|\n\
                  | `output_fps_imag` | `60` | log |\n\
                  | `power_pl1_w_imag` | `45` | MMIO RAPL PL1 ... |\n";
    let (_c, out, _e) = run_sourced(&format!(
        "imag_power_pl1_pin_from_readme_text {}",
        shell_quote(readme)
    ));
    assert_eq!(
        out.trim(),
        "45",
        "must extract the pinned 45 W from the README row (#1162 re-baseline): {out:?}"
    );
}

#[test]
fn pin_reader_empty_when_pin_absent() {
    let (_c, out, _e) = run_sourced("imag_power_pl1_pin_from_readme_text 'no pin here'");
    assert!(
        out.trim().is_empty(),
        "no pin row -> empty (caller falls back to the env default): {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_power_guard_next_streaks — the guard's own streak bookkeeping (the "2 consecutive" ledger)
// ---------------------------------------------------------------------------------------------

fn next_streaks(args: &str) -> String {
    let (_c, out, _e) = run_sourced(&format!("imag_power_guard_next_streaks {args}"));
    out.trim().to_string()
}

#[test]
fn guard_next_streaks_matches_the_two_consecutive_ledger() {
    // stepdown/restore reset both streaks and set the stepped flag.
    assert_eq!(
        next_streaks("stepdown 1 0 1 0 0"),
        "0 0 1",
        "stepdown -> reset + stepped=1"
    );
    assert_eq!(
        next_streaks("restore 0 1 0 3 1"),
        "0 0 0",
        "restore -> reset + stepped=0"
    );
    // hold on a hot read advances HOT (so the NEXT hot read is the 2nd -> stepdown).
    assert_eq!(
        next_streaks("hold 1 0 0 0 0"),
        "1 0 0",
        "first hot read -> HOT 0->1, not stepped"
    );
    // hold on a cool read advances COOL while stepped (so the NEXT cool read restores).
    assert_eq!(
        next_streaks("hold 0 1 0 0 1"),
        "0 1 1",
        "first cool read while stepped -> COOL 0->1"
    );
    // a band read (neither hot nor cool) resets both streaks.
    assert_eq!(
        next_streaks("hold 0 0 3 0 1"),
        "0 0 1",
        "hysteresis-band read -> both streaks reset"
    );
    // reassert keeps the (not-stepped) flag and advances per this read.
    assert_eq!(
        next_streaks("reassert 0 0 0 0 0"),
        "0 0 0",
        "reassert at nominal -> streaks reset, stepped unchanged"
    );
}

// ---------------------------------------------------------------------------------------------
// imag_power_alert_condition — the dev1-side watchdog pages on STEP-DOWN / RE-ASSERT, never RESTORE
// ---------------------------------------------------------------------------------------------

#[test]
fn alert_condition_fires_on_stepdown_and_reassert_but_not_restore_or_hold() {
    // A journal window with a STEP-DOWN line -> alertable.
    let (_c, out, _e) = run_sourced(
        "imag_power_alert_condition 'Aug 13 imag-nb imag-power-envelope[1]: STEP-DOWN: TCPU=94C ...'",
    );
    assert!(
        out.contains("STEP-DOWN"),
        "STEP-DOWN must be an alert condition: {out:?}"
    );

    // A RE-ASSERT line -> alertable.
    let (_c, out2, _e) = run_sourced(
        "imag_power_alert_condition 'Aug 13 imag-nb imag-power-envelope[1]: RE-ASSERT: live PL1=... foreign ...'",
    );
    assert!(
        out2.contains("RE-ASSERT"),
        "RE-ASSERT must be an alert condition: {out2:?}"
    );

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
    assert!(
        body.contains("set -euo pipefail"),
        "the oneshot must fail loud"
    );
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
    assert!(
        body.contains("set -euo pipefail"),
        "the guard must fail loud"
    );
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
    assert!(
        body.contains("set -uo pipefail"),
        "a watchdog must survive per-pass failures (not set -e)"
    );
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
fn readme_pins_power_pl1_w_imag_at_45() {
    // #1162 re-baseline: the new i7-13620H imag-nb starves the iGPU at the old 29 W; 45 W is the
    // sustainable ceiling (live calibration). The checker (verify-imag + drift-guard --check-imag)
    // reads its EXPECTED pin from this README row, so it MUST read 45 or it false-FAILs a healthy box.
    let readme = std::fs::read_to_string(manifest_dir().join("vendor/README.md")).unwrap();
    let has = readme
        .lines()
        .any(|l| l.contains("power_pl1_w_imag") && l.contains("`45`"));
    assert!(
        has,
        "vendor/README.md must pin `power_pl1_w_imag` = `45` — without the pin the drift-guard \
         PL1 facet reads UNKNOWN forever and the strict gate is inert (#1162 re-baseline)"
    );
}

// =================================================================================================
// #880 — throttle-under-floor visibility alert (the SECOND, act_freq-based alert path).
//
// The #841 pinned iGPU floor (gt_min_freq_mhz=1400) is a software REQUEST floor the hardware punit
// legally overrides at the MMIO RAPL PL1 power budget: under load `Actual freq` drops to ~750 MHz
// (moderate) / 500 MHz (heavy) with `throttle_reason_pl1=1` while every software knob still reads
// 1400 (live forcewake evidence 2026-08-15). That clamp produces NO guard STEP-DOWN journal marker
// (the guard only steps down on a TCPU excursion), so the operator gets silent judder. This alert
// path makes it VISIBLE. It keys on `throttle_reason` (NOT raw act_freq) so it does not false-fire
// on benign RC6 idle — the exact false positive the ticket body warns against.
// =================================================================================================

fn throttle_cond(gather: &str) -> String {
    let (_c, out, _e) =
        run_sourced_with_gather("imag_power_throttle_alert_condition \"$G\"", gather);
    out.trim().to_string()
}

// A burst where a MAJORITY of samples are genuinely power-clamped: act < floor AND pl1/thermal
// throttle active. The idle samples (throttle_reason all 0, act 0 = RC6 render-duty gaps) do NOT
// count. 8/12 clamped -> above the 50% default -> fires.
const CLAMP_BURST: &str = "\
FLOOR|1400
THROTSAMPLE|1|0|1|750
THROTSAMPLE|1|0|1|800
THROTSAMPLE|1|0|1|700
THROTSAMPLE|1|0|1|850
THROTSAMPLE|1|0|1|750
THROTSAMPLE|1|0|1|900
THROTSAMPLE|1|0|1|500
THROTSAMPLE|1|0|1|650
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|850
THROTSAMPLE|0|0|0|0
";

#[test]
fn throttle_alert_fires_when_a_majority_of_the_burst_is_power_clamped_under_the_floor() {
    let out = throttle_cond(CLAMP_BURST);
    assert!(
        out.contains("THROTTLE-UNDER-FLOOR"),
        "a sustained (majority-of-burst) PL1 clamp below the pinned floor must page: {out:?}"
    );
}

#[test]
fn throttle_alert_does_not_fire_on_benign_rc6_idle_even_though_act_is_below_the_floor() {
    // THE false-positive guard: every sample has act far below the floor (idle/RC6), but NO throttle
    // reason is active (status=0). This is the exact idle-sampling artifact the ticket body flags —
    // it must NOT page (that would page constantly on a resting box).
    let idle = "\
FLOOR|1400
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|350
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|300
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|450
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|550
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|400
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|350
";
    let out = throttle_cond(idle);
    assert!(
        out.is_empty(),
        "benign RC6 idle (act<floor but throttle_reason all 0) must NEVER page: {out:?}"
    );
}

#[test]
fn throttle_alert_does_not_fire_on_a_transient_minority_dip() {
    // Only 2/12 clamped -> below the 50% default -> a transient dip, not sustained -> no page.
    let transient = "\
FLOOR|1400
THROTSAMPLE|1|0|1|700
THROTSAMPLE|1|0|1|750
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|1|1400
THROTSAMPLE|0|0|0|850
THROTSAMPLE|0|0|1|1400
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|1|1400
THROTSAMPLE|0|0|0|850
THROTSAMPLE|0|0|1|1400
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|1|1400
";
    let out = throttle_cond(transient);
    assert!(
        out.is_empty(),
        "a transient minority dip (2/12) must not page — only sustained clamps do: {out:?}"
    );
}

#[test]
fn throttle_alert_fires_on_a_thermal_clamp_too_not_only_pl1() {
    // thermal=1 (pl1=0) with act below floor is equally a clamp — a majority of these pages.
    let thermal = "\
FLOOR|1400
THROTSAMPLE|0|1|1|600
THROTSAMPLE|0|1|1|650
THROTSAMPLE|0|1|1|700
THROTSAMPLE|0|1|1|600
THROTSAMPLE|0|1|1|550
THROTSAMPLE|0|1|1|650
THROTSAMPLE|0|0|0|0
THROTSAMPLE|0|0|0|0
";
    let out = throttle_cond(thermal);
    assert!(
        out.contains("THROTTLE-UNDER-FLOOR"),
        "a sustained THERMAL clamp below the floor must page too: {out:?}"
    );
}

#[test]
fn throttle_alert_ignores_samples_at_or_above_the_floor_even_with_a_throttle_flag() {
    // Every sample carries pl1=1 but act == floor (1400) -> the floor IS being met -> not a clamp
    // sample -> no page. `act < floor` is the hard gate, not the throttle flag alone.
    let atfloor = "\
FLOOR|1400
THROTSAMPLE|1|0|1|1400
THROTSAMPLE|1|0|1|1400
THROTSAMPLE|1|0|1|1400
THROTSAMPLE|1|0|1|1400
THROTSAMPLE|1|0|1|1400
THROTSAMPLE|1|0|1|1400
";
    let out = throttle_cond(atfloor);
    assert!(
        out.is_empty(),
        "act at/above the floor is not a clamp, even with a throttle flag: {out:?}"
    );
}

#[test]
fn throttle_alert_empty_or_missing_floor_never_pages_no_false_alert_on_ssh_hiccup() {
    assert!(
        throttle_cond("").is_empty(),
        "empty burst (ssh failure) -> nothing to decide, never a false alert"
    );
    // Clamped-looking samples but NO floor line -> cannot judge -> no page.
    let nofloor = "\
THROTSAMPLE|1|0|1|750
THROTSAMPLE|1|0|1|700
THROTSAMPLE|1|0|1|800
";
    assert!(
        throttle_cond(nofloor).is_empty(),
        "no FLOOR line -> cannot compare -> never a false alert: {:?}",
        throttle_cond(nofloor)
    );
}

// -------------------------------------------------------------------------------------------------
// imag_power_throttle_burst_remote_snippet — the burst gather, identity-based, separate from the
// instantaneous shared gather so drift-guard/verify-imag are not slowed by a multi-second burst.
// -------------------------------------------------------------------------------------------------

#[test]
fn throttle_burst_snippet_samples_throttle_reason_and_act_by_card_identity() {
    let (_c, out, _e) = run_sourced("imag_power_throttle_burst_remote_snippet");
    assert!(
        out.contains("throttle_reason_pl1")
            && out.contains("throttle_reason_thermal")
            && out.contains("throttle_reason_status"),
        "the burst must read the pl1/thermal/status throttle reasons: {out:?}"
    );
    assert!(
        out.contains("rps_act_freq_mhz") && out.contains("rps_min_freq_mhz"),
        "the burst must read act freq + the min-freq floor: {out:?}"
    );
    assert!(
        out.contains("card*"),
        "the burst must glob card* (never a hardcoded cardN — presenter-drm renumbering hazard): {out:?}"
    );
    assert!(
        out.contains("THROTSAMPLE") && out.contains("FLOOR"),
        "the burst must emit THROTSAMPLE lines + a FLOOR line the pure condition parses: {out:?}"
    );
    assert!(
        out.contains("sleep"),
        "the burst must sample repeatedly over time (a sleep loop), not once: {out:?}"
    );
}

// -------------------------------------------------------------------------------------------------
// the dev1 watchdog wires the SECOND alert path in (shared condition + shared throttle + burst)
// -------------------------------------------------------------------------------------------------

#[test]
fn dev1_alert_watchdog_wires_in_the_throttle_under_floor_path() {
    let body = read_script("scripts/imag-power-envelope-alert-watchdog.sh");
    assert!(
        body.contains("imag_power_throttle_alert_condition"),
        "the watchdog must ALSO decide the throttle-under-floor condition via the shared pure fn"
    );
    assert!(
        body.contains("imag_power_throttle_burst_remote_snippet"),
        "the watchdog must gather the throttle burst via the shared snippet"
    );
}

#[test]
fn throttle_alert_does_not_fire_on_a_truncated_partial_burst() {
    // ssh dropped mid-burst -> only 2 samples captured, both clamped. 2/2 = 100% >= 50%, but the
    // burst emits ~12; a 2-sample capture is NOT evidence of a SUSTAINED clamp -> must not page
    // (the min-sample floor, default 6).
    let truncated = "\
FLOOR|1400
THROTSAMPLE|1|0|1|700
THROTSAMPLE|1|0|1|750
";
    let out = throttle_cond(truncated);
    assert!(
        out.is_empty(),
        "a truncated 2-sample burst must not read as sustained (min-sample guard): {out:?}"
    );
}

#[test]
fn throttle_alert_sig_is_stable_across_bursts_with_different_counts() {
    // The dedup signature must NOT embed the fluctuating clamped/total count, or one ongoing clamp
    // re-pages every pass instead of once-then-suppress. Two different marker lines from the SAME
    // episode must yield the SAME signature.
    let (_c, s1, _e) = run_sourced(
        "imag_power_throttle_alert_sig 'THROTTLE-UNDER-FLOOR: 8/12 burst samples held act<1400MHz while PL1/thermal-clamped (threshold 50%)'",
    );
    let (_c, s2, _e) = run_sourced(
        "imag_power_throttle_alert_sig 'THROTTLE-UNDER-FLOOR: 11/12 burst samples held act<1400MHz while PL1/thermal-clamped (threshold 50%)'",
    );
    assert_eq!(
        s1.trim(),
        s2.trim(),
        "the dedup sig must be stable across differing clamp counts within one episode: {s1:?} vs {s2:?}"
    );
    assert!(
        !s1.trim().is_empty(),
        "the sig must be a non-empty stable token: {s1:?}"
    );
}

// =================================================================================================
// #799 — the render-degradation CAUSE discriminator.
//
// Two distinct causes produce the same "OBS render budget blown after hours, restart clears it"
// symptom on imag-nb: (a) the issue-880/1043 power/thermal clamp (GPU steered below the pinned
// floor, throttle_reason_pl1/thermal active) and (b) THIS ticket's connection-churn render leak
// (render time creeps while the GPU has HEADROOM — throttle clean). The salvaged 2026-08-16 plan:
// capture render stats + gt_act_freq + throttle_reason SIMULTANEOUSLY and NAME which cause is
// active, instead of one ambiguous "render degraded" alert. These pin the pure classifiers:
//   imag_render_degraded_from_sample  — RENDER|<afps>|<avg_ms>|<skip_frac>|<adv> -> degraded|healthy|stalled|unknown
//   imag_power_throttle_state         — the burst -> clamped|clean|unknown (3-state; shares ONE parse
//                                       with the existing 2-state imag_power_throttle_alert_condition)
//   imag_render_cause_from_signals    — fuses them -> <cause>|<detail> (churn-leak|power-clamp|healthy|stalled|unknown)
// Thresholds mirror src/render_budget.rs (60fps: budget 1000/60=16.67ms, fps floor 58, skip 5%),
// and activeFps is only trusted when render_advanced=true (#935: activeFps LIES during a stall).
// =================================================================================================

fn render_class(line: &str) -> String {
    let (_c, out, _e) = run_sourced(&format!(
        "imag_render_degraded_from_sample {}",
        shell_quote(line)
    ));
    out.trim().to_string()
}

fn throttle_state(gather: &str) -> String {
    let (_c, out, _e) = run_sourced_with_gather("imag_power_throttle_state \"$G\"", gather);
    out.trim().to_string()
}

fn cause(line: &str, gather: &str) -> String {
    let (_c, out, _e) = run_sourced_with_gather(
        &format!(
            "imag_render_cause_from_signals {} \"$G\"",
            shell_quote(line)
        ),
        gather,
    );
    out.trim().to_string()
}

fn cause_token(line: &str, gather: &str) -> String {
    cause(line, gather)
        .lines()
        .next()
        .unwrap_or("")
        .split('|')
        .next()
        .unwrap_or("")
        .to_string()
}

// A CLEAN burst: valid FLOOR, >= min samples, NONE clamped (act at/above floor, throttle all 0) =
// the GPU has headroom. This is the churn-discriminator's key "throttle clean" input.
const CLEAN_BURST: &str = "\
FLOOR|1400
THROTSAMPLE|0|0|0|1400
THROTSAMPLE|0|0|0|1400
THROTSAMPLE|0|0|0|1350
THROTSAMPLE|0|0|0|1400
THROTSAMPLE|0|0|0|1400
THROTSAMPLE|0|0|0|1400
THROTSAMPLE|0|0|0|1350
THROTSAMPLE|0|0|0|1400
";

#[test]
fn render_sample_healthy_within_the_60fps_budget() {
    // 60fps, 5.3ms (< 16.67 budget), 0% skip, advancing -> healthy.
    assert_eq!(render_class("RENDER|60.00|5.30|0.000|true"), "healthy");
}

#[test]
fn render_sample_degraded_by_avg_render_time_the_799_curve() {
    // The ticket's own curve: 52.8fps / 17.2ms / 1.6% skip -> the avg exceeds the 16.67ms budget.
    assert_eq!(render_class("RENDER|52.80|17.20|0.016|true"), "degraded");
}

#[test]
fn render_sample_degraded_by_low_fps_only_when_advancing() {
    // avg under budget, skip low, but activeFps sags below the 58 floor AND renderTotalFrames is
    // confirmed advancing (=true) -> trust the fps signal -> degraded.
    assert_eq!(render_class("RENDER|55.00|10.00|0.000|true"), "degraded");
}

#[test]
fn render_sample_degraded_by_render_skip_over_tolerance() {
    // 8% skip > the 5% tolerance -> degraded.
    assert_eq!(render_class("RENDER|60.00|10.00|0.080|true"), "degraded");
}

#[test]
fn render_sample_full_stall_is_stalled_not_degraded_defers_to_391() {
    // render_advanced=false = a FULL render-loop stall (activeFps LIES here, #935). That is the
    // #391 obs-liveness FpsZero path's domain, NOT this partial-degrade discriminator -> `stalled`
    // (so the two watchdogs never double-alert the same stall).
    assert_eq!(render_class("RENDER|30.00|0.00|0.000|false"), "stalled");
}

#[test]
fn render_sample_low_fps_not_trusted_when_advancement_unknown_the_activefps_lie_guard() {
    // renderTotalFrames advancement could NOT be confirmed (adv=unknown) and avg/skip are healthy:
    // a low activeFps here may be the #935 lie, so it must NOT alone mark the box degraded.
    assert_eq!(render_class("RENDER|30.00|5.00|0.000|unknown"), "healthy");
}

#[test]
fn render_sample_unknown_on_empty_or_malformed_never_a_false_signal() {
    assert_eq!(render_class(""), "unknown");
    assert_eq!(render_class("RENDER|x|y|z|true"), "unknown"); // non-numeric avg
    assert_eq!(render_class("GARBAGE|60|5|0|true"), "unknown"); // wrong tag
}

#[test]
fn throttle_state_reports_clamped_clean_unknown_three_ways() {
    assert_eq!(throttle_state(CLAMP_BURST), "clamped"); // majority power-clamped under floor
    assert_eq!(throttle_state(CLEAN_BURST), "clean"); // valid burst, GPU has headroom
    assert_eq!(throttle_state(""), "unknown"); // empty (ssh hiccup)
                                               // truncated (2 samples < min 6) -> cannot judge -> unknown, never a false clean/clamped.
    assert_eq!(
        throttle_state("FLOOR|1400\nTHROTSAMPLE|1|0|1|700\nTHROTSAMPLE|1|0|1|750\n"),
        "unknown"
    );
    // no FLOOR line -> unknown.
    assert_eq!(
        throttle_state("THROTSAMPLE|0|0|0|1400\nTHROTSAMPLE|0|0|0|1400\n"),
        "unknown"
    );
}

#[test]
fn throttle_state_shares_one_parse_with_the_two_state_alert_condition() {
    // The refactor must not regress the existing 2-state marker: clamped burst still yields the
    // THROTTLE-UNDER-FLOOR marker, a clean burst yields none.
    assert!(throttle_cond(CLAMP_BURST).contains("THROTTLE-UNDER-FLOOR"));
    assert!(throttle_cond(CLEAN_BURST).is_empty());
}

#[test]
fn cause_render_degraded_with_clean_throttle_is_churn_leak_799() {
    // THE central case: render degraded (17.2ms) while the GPU has headroom (throttle clean) =
    // the #799 connection-churn leak, which an OBS restart clears — NOT the power clamp.
    let out = cause("RENDER|52.80|17.20|0.016|true", CLEAN_BURST);
    assert_eq!(
        cause_token("RENDER|52.80|17.20|0.016|true", CLEAN_BURST),
        "churn-leak"
    );
    assert!(
        out.contains("#799") && (out.contains("headroom") || out.contains("restart")),
        "the churn-leak detail must name #799 and the restart-clears/headroom mechanism: {out:?}"
    );
}

#[test]
fn cause_render_degraded_with_clamped_throttle_is_power_clamp_not_churn() {
    // render degraded WHILE the iGPU is power-clamped -> attribute to the power/cooling envelope
    // (issue 880/1043), never cry churn (the throttle path already pages the clamp).
    assert_eq!(
        cause_token("RENDER|52.80|17.20|0.016|true", CLAMP_BURST),
        "power-clamp"
    );
}

#[test]
fn cause_render_degraded_with_unknown_throttle_cannot_attribute() {
    // render degraded but the burst is unreadable (ssh hiccup) -> cannot discriminate -> unknown,
    // never a false churn blame.
    assert_eq!(cause_token("RENDER|52.80|17.20|0.016|true", ""), "unknown");
}

#[test]
fn cause_healthy_render_never_alerts_regardless_of_throttle() {
    assert_eq!(
        cause_token("RENDER|60.00|5.30|0.000|true", CLAMP_BURST),
        "healthy"
    );
    assert_eq!(
        cause_token("RENDER|60.00|5.30|0.000|true", CLEAN_BURST),
        "healthy"
    );
}

#[test]
fn cause_full_stall_defers_regardless_of_throttle() {
    assert_eq!(
        cause_token("RENDER|30.00|0.00|0.000|false", CLEAN_BURST),
        "stalled"
    );
}

// -------------------------------------------------------------------------------------------------
// The dev1 watchdog wires the render-discriminator path in (render read + fusion + confirm + dedup),
// and the churn-leak alert names #799. Reuses obs_watchdog_confirm (2-pass) so a single transient
// render window never pages; power-clamp is left to the existing throttle path (no duplicate page).
// -------------------------------------------------------------------------------------------------

#[test]
fn dev1_alert_watchdog_wires_in_the_render_discriminator_799() {
    let body = read_script("scripts/imag-power-envelope-alert-watchdog.sh");
    assert!(
        body.contains("imag_render_cause_from_signals"),
        "the watchdog must decide the render CAUSE via the shared pure fusion fn"
    );
    assert!(
        body.contains("imag-render-stats.py"),
        "the watchdog must read imag OBS render stats over WS via the dev1-side reader front"
    );
    assert!(
        body.contains("churn-leak"),
        "the watchdog must page the NEW previously-silent churn-leak case"
    );
    assert!(
        body.contains("#799"),
        "the churn-leak alert must name #799 so the operator/next-investigator knows the cause"
    );
    assert!(
        body.contains("obs_watchdog_confirm"),
        "the render path must confirm across >=2 passes (a single 4s window can catch a transient)"
    );
}

// ---------------------------------------------------------------------------------------------
// #1188 — the guard's /run state PATH constant + the pure STEPPED parser (the acceptance gate,
// scripts/verify-imag.sh, consults these to tell a LEGITIMATE thermal step-down from foreign drift)
// ---------------------------------------------------------------------------------------------

#[test]
fn guard_state_path_is_one_shared_constant_1188() {
    // Both the guard (the writer) and verify-imag (the reader) must agree on the /run state path,
    // so the lib exports it as ONE constant rather than each hardcoding its own literal.
    let (_c, out, err) = run_sourced(r#"printf '%s\n' "${IMAG_POWER_GUARD_STATE_FILE:-UNSET}""#);
    assert_eq!(
        out.trim(),
        "/run/imag-power-envelope-guard.state",
        "the lib must export IMAG_POWER_GUARD_STATE_FILE = the guard's /run state path (#1188): out={out:?} err={err:?}"
    );
    // And the guard script must reference the shared constant, never a second literal copy.
    let guard = read_script("scripts/imag-power-envelope-guard.sh");
    assert!(
        guard.contains("IMAG_POWER_GUARD_STATE_FILE"),
        "the guard must use the shared IMAG_POWER_GUARD_STATE_FILE constant for its state path (#1188)"
    );
}

#[test]
fn guard_stepped_from_state_reads_the_stepped_flag_1188() {
    // STEPPED=1 present -> stepped; STEPPED=0/other -> not-stepped; empty or no STEPPED= -> unknown
    // (never mask a genuine drift when the guard state cannot be confirmed).
    let cases = [
        ("HOT=2\nCOOL=0\nSTEPPED=1", "stepped"),
        ("HOT=0\nCOOL=3\nSTEPPED=0", "not-stepped"),
        ("STEPPED=1\n", "stepped"),
        ("STEPPED=0\n", "not-stepped"),
        // a stray CR / surrounding whitespace on the value must still read as stepped.
        ("HOT=1\nCOOL=0\nSTEPPED= 1 \n", "stepped"),
        // absent file / empty read -> unknown.
        ("", "unknown"),
        // present but no STEPPED= line (truncated/corrupt) -> unknown, not a false not-stepped.
        ("HOT=1\nCOOL=0\n", "unknown"),
        // a garbage STEPPED value with no digit -> not-stepped (safe: keeps the strict fail).
        ("STEPPED=x", "not-stepped"),
    ];
    for (text, want) in cases {
        let (_c, out, err) = run_sourced(&format!(
            "imag_power_guard_stepped_from_state {}",
            shell_quote(text)
        ));
        assert_eq!(
            out.trim(),
            want,
            "imag_power_guard_stepped_from_state({text:?}) -> want {want:?}: out={out:?} err={err:?}"
        );
    }
}

#[test]
fn guard_state_parser_returns_zero_on_every_input_never_aborts_a_set_e_caller_1188() {
    // verify-imag.sh (set -euo pipefail) calls this in a `$(...)`; it must ALWAYS exit 0 so an empty
    // / malformed read never set -e-aborts the whole acceptance gate (#1133 class).
    let (c, out, err) = run_sourced(
        "set -e\n\
         imag_power_guard_stepped_from_state '' >/dev/null; echo rc-empty=$?\n\
         imag_power_guard_stepped_from_state 'HOT=1' >/dev/null; echo rc-nostep=$?\n\
         imag_power_guard_stepped_from_state 'STEPPED=1' >/dev/null; echo rc-stepped=$?",
    );
    assert_eq!(c, 0, "harness must not abort: out={out:?} err={err:?}");
    for line in ["rc-empty=0", "rc-nostep=0", "rc-stepped=0"] {
        assert!(out.contains(line), "expected {line}: out={out:?}");
    }
}

#[test]
fn guard_writes_its_state_file_world_readable_1188() {
    // The guard runs as root and mktemp yields mode 600 — but verify-imag consults STEPPED over a
    // NON-root SSH (newlevel), so the guard must chmod the state file world-readable before the
    // atomic mv. Without it, the acceptance gate can never read the guard state and would keep
    // false-FAILing a legitimate step-down (#1188).
    let body = read_script("scripts/imag-power-envelope-guard.sh");
    assert!(
        body.contains("chmod 0644"),
        "the guard must chmod its state file 0644 so the non-root verify SSH can read STEPPED (#1188)"
    );
    // It must also RECORD its step-down watts so verify compares against the guard's own authority,
    // not an independent env default (#1188).
    assert!(
        body.contains("GUARD_STEPDOWN_W="),
        "the guard must write GUARD_STEPDOWN_W into its state file so verify reads the guard's OWN step-down value (#1188)"
    );
}

#[test]
fn guard_stepdown_w_from_state_reads_the_recorded_value_1188() {
    // GUARD_STEPDOWN_W present -> its digits; absent/empty -> empty (verify then falls back to the
    // env default). Always returns 0 (called inside a `$(...)` under set -euo pipefail).
    let cases = [
        ("HOT=0\nCOOL=0\nSTEPPED=1\nGUARD_STEPDOWN_W=25", "25"),
        ("GUARD_STEPDOWN_W= 25 \n", "25"),
        // absent -> empty
        ("HOT=1\nCOOL=0\nSTEPPED=1", ""),
        ("", ""),
    ];
    for (text, want) in cases {
        let (_c, out, err) = run_sourced(&format!(
            "imag_power_guard_stepdown_w_from_state {}",
            shell_quote(text)
        ));
        assert_eq!(
            out.trim(),
            want,
            "imag_power_guard_stepdown_w_from_state({text:?}) -> want {want:?}: out={out:?} err={err:?}"
        );
    }
    // set -e contract: an empty/absent read must not abort a set -euo pipefail caller.
    let (c, out, err) = run_sourced(
        "set -e\n\
         imag_power_guard_stepdown_w_from_state '' >/dev/null; echo rc-empty=$?\n\
         imag_power_guard_stepdown_w_from_state 'GUARD_STEPDOWN_W=25' >/dev/null; echo rc-set=$?",
    );
    assert_eq!(c, 0, "harness must not abort: out={out:?} err={err:?}");
    for line in ["rc-empty=0", "rc-set=0"] {
        assert!(out.contains(line), "expected {line}: out={out:?}");
    }
}
