//! #1072 — E2E cleanup repeatedly left `cam2-painter.service` DEAD, killing later gate runs at
//! the `[0/8]` preflight ("optical injection leg DEAD"). Three previously-separate seams are
//! hardened here, each with its own regression test:
//!
//!   1. **cleanup restore = RETRY + fail-loud + CONDITIONAL dead-man disarm.** The old restore was
//!      a one-shot `systemctl start cam2-painter 2>/dev/null || true` whose failure was swallowed,
//!      followed by an UNCONDITIONAL `cam2_painter_deadman_disarm_cmds` — so a failed restore left
//!      the painter dead AND tore down the on-box self-heal net. The retry (a new #675 sourced-lib
//!      builder) tries again, fails LOUD, and the disarm is now guarded by the retry's success flag
//!      so a failed restore leaves the dead-man ARMED.
//!   2. **dead-man window unified to a PERIODIC ~5-min re-fire.** The old one-shot `--on-active=90min`
//!      left a SIGKILLed run's painter dark for up to 90 min. A periodic timer (`--on-unit-active`)
//!      with a ~5-min window re-fires until the painter is back; the existing `pgrep -x frame-probe`
//!      guard keeps every fire a no-op during a live run, so the short window never races the run.
//!   3. **[0/8] preflight makes EXACTLY ONE self-heal `systemctl start` + re-probe before refusing**
//!      (fail-closed): a standing painter a previous run left dead is given one recovery attempt
//!      instead of wasting the run outright.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------------------------
// (1) cleanup: retry builder is sourced + wired in, and the dead-man disarm is CONDITIONAL.
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_restore_retry_lib_1072() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/cam2-painter-restore-retry.sh\""),
        "#1072: recording-e2e.sh must source scripts/lib/cam2-painter-restore-retry.sh — the \
         retry text is single-sourced there (the #675 pattern), never inlined at the anchored \
         cleanup restore line"
    );
}

#[test]
fn cleanup_calls_restore_retry_after_the_one_shot_start_1072() {
    let s = read("scripts/recording-e2e.sh");
    let start = s
        .find("systemctl start cam2-painter 2>/dev/null || true")
        .expect(
        "#1072: the existing one-shot start (attempt #1) must remain as the retry's first attempt",
    );
    let retry = s
        .find("$(cam2_painter_restore_retry_cmds)")
        .unwrap_or_else(|| {
            panic!(
                "#1072: cleanup() must call the retry builder so the restore is more than one-shot"
            )
        });
    assert!(
        retry > start,
        "#1072: the retry (attempts 2..N) must follow the first start attempt (start {start}, retry {retry})"
    );
}

#[test]
fn cleanup_disarm_is_guarded_by_the_restore_success_flag_1072() {
    let s = read("scripts/recording-e2e.sh");
    let retry = s
        .find("$(cam2_painter_restore_retry_cmds)")
        .expect("#1072: retry call must exist");
    let disarm = s[retry..]
        .find("cam2_painter_deadman_disarm_cmds")
        .map(|i| retry + i)
        .expect("#1072: cleanup() must still disarm the dead-man after the restore");
    let between = &s[retry..disarm];
    assert!(
        between.contains("_cprr_ok"),
        "#1072: the dead-man disarm must be GUARDED by the retry's success flag (_cprr_ok) so a \
         FAILED restore leaves the dead-man ARMED for the ~5-min on-box self-heal; got text \
         between the retry and the disarm:\n{between}"
    );
}

// ---------------------------------------------------------------------------------------------
// (2) dead-man: PERIODIC re-fire, short window, guard retained.
// ---------------------------------------------------------------------------------------------

#[test]
fn deadman_is_a_periodic_refire_timer_1072() {
    let s = read("scripts/lib/cam2-painter-deadman.sh");
    assert!(
        s.contains("--on-unit-active"),
        "#1072: the dead-man must be a PERIODIC timer (systemd-run --on-unit-active) so it \
         re-fires and heals a standing painter within the window on ANY exit path — a one-shot \
         --on-active fires once and cannot recover a run killed after it already fired"
    );
}

#[test]
fn deadman_window_is_short_enough_for_a_5min_recovery_1072() {
    let s = read("scripts/lib/cam2-painter-deadman.sh");
    let line = s
        .lines()
        .find(|l| l.starts_with("CAM2_PAINTER_DEADMAN_MINUTES="))
        .expect("#1072: expected the window default");
    let mins: u32 = line
        .split(":-")
        .nth(1)
        .and_then(|t| t.trim_end_matches("}\"").trim_end_matches('}').parse().ok())
        .unwrap_or_else(|| panic!("#1072: could not parse the window from {line:?}"));
    assert!(
        mins <= 5,
        "#1072: a standing TEST painter must never be dark longer than ~5 min; the periodic \
         dead-man window is {mins} min. Mid-run safety comes from the pgrep-frame-probe guard \
         (a periodic fire during a live run is a no-op), NOT from a long one-shot delay."
    );
}

#[test]
fn deadman_keeps_the_frame_probe_guard_1072() {
    let s = read("scripts/lib/cam2-painter-deadman.sh");
    assert!(
        s.contains("pgrep -x frame-probe"),
        "#1072: shortening the window to ~5 min relies on the pgrep guard to no-op every fire \
         during a live run (frame-probe is running the whole run) — it must stay, or a periodic \
         fire mid-run would start a second painter (#440)"
    );
}

// ---------------------------------------------------------------------------------------------
// (3) [0/8] preflight: EXACTLY ONE self-heal `systemctl start` + re-probe before the abort.
// ---------------------------------------------------------------------------------------------

#[test]
fn preflight_attempts_one_self_heal_start_before_the_abort_1072() {
    let s = read("scripts/lib/optical-chain-preflight.sh");
    let heal = s
        .find("systemctl start")
        .unwrap_or_else(|| panic!("#1072: the preflight must attempt ONE self-heal `systemctl start` of the painter before refusing the run"));
    // The preflight file carries OTHER checks with their own `exit 1` lines BEFORE the painter
    // block (the first `.find("exit 1")` landed on one of those on the merged tree — the
    // anchor-uniqueness trap from the project CLAUDE.md). The intent is: a fail-closed abort
    // exists AFTER the one self-heal attempt — so anchor the search FROM the heal site.
    assert!(
        s[heal..].contains("exit 1"),
        "#1072: the preflight must still fail-closed with exit 1 AFTER the self-heal attempt (heal at {heal})"
    );
}

// --- functional: fake systemctl/ssh/sshpass stand-ins prove the self-heal is tried EXACTLY once
//     and the abort is fail-closed, without any live ssh/rig dependency (mirrors the #863
//     run_against_fakes pattern). ---

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "optical-preflight-selfheal-1072-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_fake_bin(dir: &Path, name: &str, script: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/usr/bin/env bash\n{script}\n")).expect("write fake bin");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&path, perms).unwrap();
}

/// Run `optical_chain_preflight_assert` against fake systemctl/ssh/sshpass/sleep/python3, with the
/// painter EXPECTED (service enabled) but initially DEAD (inactive). `mode` = "success" → the
/// self-heal `systemctl start` brings it active; "fail" → start never takes. Returns the assert's
/// exit code and the number of `systemctl start` calls recorded.
fn run_preflight_against_fakes(mode: &str) -> (i32, usize) {
    let bin = scratch(&format!("bin-{mode}"));
    let state = scratch(&format!("state-{mode}"));

    // sshpass -p PW ssh ...  ->  drop `-p PW`, exec the rest (the ssh invocation).
    write_fake_bin(&bin, "sshpass", "shift 2\nexec \"$@\"");
    // ssh ... user@host "REMOTE"  ->  run the last arg (the remote command) under bash with the
    // fake PATH inherited.
    write_fake_bin(&bin, "ssh", "cmd=\"${@: -1}\"\nexec bash -c \"$cmd\"");
    // systemctl: is-enabled -> enabled (expected); is-active -> active iff the state flag exists;
    // start -> record + (success mode only) create the active flag.
    write_fake_bin(
        &bin,
        "systemctl",
        "sub=\"$1\"\ncase \"$sub\" in\n\
         is-enabled) echo enabled ;;\n\
         is-active) if [ -f \"$CB_STATE/active\" ]; then echo active; else echo inactive; fi ;;\n\
         start) echo start >> \"$CB_STATE/starts\"; if [ \"$CB_MODE\" = success ]; then : > \"$CB_STATE/active\"; fi ;;\n\
         list-unit-files) exit 0 ;;\n\
         reset-failed) exit 0 ;;\n\
         *) exit 0 ;;\n\
         esac",
    );
    write_fake_bin(&bin, "sleep", "exit 0"); // never actually wait
    write_fake_bin(&bin, "python3", "exit 0"); // obs_phase2 assert-program-nonblack -> NON-BLACK

    let repo = root();
    let harness = format!(
        r#"set -uo pipefail
export PATH="{bin}:$PATH"
export CB_STATE="{state}"
export CB_MODE="{mode}"
. "{repo}/scripts/lib/optical-chain-health.sh"
. "{repo}/scripts/lib/optical-chain-preflight.sh"
optical_chain_preflight_assert "1.2.3.4" root "pw" "5.6.7.8" "obspw" "{repo}/scripts" "{state}/nonexistent.pid" "cam2-painter.service"
"#,
        bin = bin.display(),
        state = state.display(),
        mode = mode,
        repo = repo.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    let starts = fs::read_to_string(state.join("starts"))
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    (out.status.code().unwrap_or(-1), starts)
}

#[test]
fn preflight_self_heal_recovers_a_dead_painter_and_proceeds_1072() {
    let (code, starts) = run_preflight_against_fakes("success");
    assert_eq!(
        code, 0,
        "#1072: when the painter is EXPECTED+DEAD and ONE `systemctl start` brings it back, the \
         preflight must self-heal and PROCEED (exit 0), not waste the run"
    );
    assert_eq!(
        starts, 1,
        "#1072: the self-heal must be attempted EXACTLY once (got {starts} `systemctl start` calls)"
    );
}

#[test]
fn preflight_self_heal_is_fail_closed_when_it_does_not_take_1072() {
    let (code, starts) = run_preflight_against_fakes("fail");
    assert_eq!(
        code, 1,
        "#1072: when the ONE self-heal `systemctl start` does not bring the painter back, the \
         preflight must fail-closed with exit 1 (not proceed on a dead optical leg)"
    );
    assert_eq!(
        starts, 1,
        "#1072: the self-heal must be tried EXACTLY once even on failure (got {starts} calls) — \
         no unbounded retry loop that could hang the preflight"
    );
}
