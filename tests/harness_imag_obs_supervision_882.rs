//! #882 — imag-nb's OBS supervision (systemd Restart=on-failure), core-dump enablement, and the
//! wallpaper-refresh alert. Content/wiring tests only — no rig, no OBS, no ssh.
//!
//! Background: imag-nb's OBS is launched from the openbox autostart with NOTHING supervising it
//! (no systemd unit at all before this ticket) — a segfault left the audience-facing projection
//! dark for ~70 minutes with nothing paging anyone. `imag-wallpaper-refresh.service` (a pre-
//! existing 5-min user timer, previously untracked in this repo — the same "hand-installed on the
//! live box, never provisioned" gap #840 already documented for imag-obs-start.sh/stop.sh)
//! already logs "obs not running — keeping existing fallback" every 5 minutes; wiring THAT
//! existing detection to the same #391 alert path is the load-bearing, cheapest fix.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const OBS_UNIT: &str = "systemd/imag-obs.service";
const WALLPAPER_SERVICE: &str = "systemd/imag-wallpaper-refresh.service";
const WALLPAPER_TIMER: &str = "systemd/imag-wallpaper-refresh.timer";
const WALLPAPER_SCRIPT: &str = "scripts/imag-wallpaper-refresh.sh";
const SETUP: &str = "scripts/setup-imag.sh";

// ================================================================================================
// systemd/imag-obs.service — the new supervision unit
// ================================================================================================

#[test]
fn imag_obs_service_restarts_only_on_failure_never_always() {
    let body = read(OBS_UNIT);
    assert!(
        body.contains("Restart=on-failure"),
        "imag-obs.service must use Restart=on-failure (segfault self-heal), got:\n{body}"
    );
    assert!(
        !body.contains("Restart=always"),
        "imag-obs.service must NEVER use Restart=always -- that restarts even a clean exit(0), \
         reintroducing the stood-down watchdog's operator-fighting bug (issue 788)"
    );
}

#[test]
fn imag_obs_service_uses_the_existing_start_and_stop_scripts_unchanged() {
    let body = read(OBS_UNIT);
    assert!(
        body.contains("ExecStart=/usr/local/bin/imag-obs-start.sh"),
        "must reuse imag-obs-start.sh as ExecStart -- never a duplicated inline launch"
    );
    assert!(
        body.contains("ExecStop=/usr/local/bin/imag-obs-stop.sh --exec-stop"),
        "must reuse imag-obs-stop.sh (in --exec-stop mode) as ExecStop -- never a duplicated \
         graceful-close/SIGTERM/SIGKILL ladder"
    );
}

#[test]
fn imag_obs_service_enables_core_dumps() {
    let body = read(OBS_UNIT);
    assert!(
        body.contains("LimitCORE=infinity"),
        "imag-obs.service must set LimitCORE=infinity -- ulimit -c was 0 on imag-nb, leaving the \
         2026-07-30 segfault with nothing debuggable"
    );
}

#[test]
fn imag_obs_service_is_type_simple() {
    let body = read(OBS_UNIT);
    assert!(
        body.contains("Type=simple"),
        "imag-obs.service must be Type=simple -- imag-obs-start.sh now waits on obs's own pid and \
         propagates its exit status, so the wrapper script IS the correctly-tracked main process"
    );
}

// ================================================================================================
// systemd/imag-wallpaper-refresh.{service,timer} — previously untracked in this repo (the SAME
// provisioning gap #840 documented for imag-obs-start.sh/stop.sh: hand-installed on the live box,
// never fetched/written by setup-imag.sh).
// ================================================================================================

#[test]
fn wallpaper_refresh_unit_files_exist_and_match_the_live_box() {
    let service = read(WALLPAPER_SERVICE);
    let timer = read(WALLPAPER_TIMER);
    assert!(service.contains("ExecStart=/usr/local/bin/imag-wallpaper-refresh.sh"));
    assert!(timer.contains("OnUnitActiveSec=5min"));
    assert!(timer.contains("WantedBy=timers.target"));
}

// ================================================================================================
// scripts/imag-wallpaper-refresh.sh — the alert wiring (#882 item 2)
// ================================================================================================

#[test]
fn wallpaper_refresh_sources_the_391_decision_lib_never_a_new_poller() {
    let body = read(WALLPAPER_SCRIPT);
    assert!(
        body.contains("obs-watchdog-decision.sh"),
        "must reuse the #391 pure decision functions (obs_watchdog_confirm / \
         obs_watchdog_alert_throttle) -- never invent a second poller/alert mechanism"
    );
}

#[test]
fn wallpaper_refresh_alerts_via_the_same_notify_path_as_391() {
    let body = read(WALLPAPER_SCRIPT);
    assert!(
        body.contains("airuleset.py"),
        "must fire through the SAME airuleset.py notify path #391's watchdog already uses, \
         never a hand-rolled Discord call"
    );
}

#[test]
fn wallpaper_refresh_still_keeps_the_existing_fallback_when_obs_is_down() {
    let body = read(WALLPAPER_SCRIPT);
    assert!(
        body.contains("pgrep -x obs >/dev/null || {"),
        "the existing 'keep the last good fallback image, never touch it while OBS is down' guard \
         must be UNCHANGED -- the alert is an ADDITION, not a replacement"
    );
}

// ================================================================================================
// scripts/setup-imag.sh — provisioning wiring (a script hand-installed on the live box, never
// provisioned by setup-imag.sh, is a fresh-reprovision gap the NEXT box will silently repeat --
// same #840 lesson).
// ================================================================================================

#[test]
fn setup_imag_provisions_the_obs_supervision_unit() {
    let body = read(SETUP);
    assert!(
        body.contains("imag-obs.service"),
        "setup-imag.sh must provision systemd/imag-obs.service onto a fresh box (#882)"
    );
}

#[test]
fn setup_imag_provisions_the_wallpaper_refresh_unit_and_script() {
    let body = read(SETUP);
    assert!(
        body.contains("imag-wallpaper-refresh.sh"),
        "setup-imag.sh must fetch+install imag-wallpaper-refresh.sh onto a fresh box (#882) -- it \
         was never provisioned before (hand-installed only, the same #840-class gap)"
    );
    assert!(
        body.contains("imag-wallpaper-refresh.timer"),
        "setup-imag.sh must provision the wallpaper-refresh timer too, not just the script"
    );
}

#[test]
fn setup_imag_installs_systemd_coredump() {
    let body = read(SETUP);
    assert!(
        body.contains("systemd-coredump"),
        "setup-imag.sh must install systemd-coredump so LimitCORE=infinity's captured cores \
         actually land somewhere inspectable (kernel.core_pattern was a bare non-piped 'core' \
         before this, #882)"
    );
}
