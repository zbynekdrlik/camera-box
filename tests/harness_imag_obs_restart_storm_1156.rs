//! #1156 — dev1-side imag-obs restart-STORM detector, folded into the existing #882
//! `scripts/imag-obs-alert-watchdog.sh` (never a second prober) behind a default-OFF env flag.
//!
//! Background: the #1143 record-encoder lane added `import imag_record_encoder` to imag_scenes.py
//! but never added the sibling to setup-imag.sh's on-box install list, so a deploy pushed the
//! importer without the imported module → every imag-obs-start.sh seed died on ModuleNotFoundError
//! → `Restart=on-failure` relaunched the cgroup → 1737 restarts / 8.5h, and NOTHING read
//! imag-obs.service's `NRestarts` so it paged nobody. This detector closes the alert gap.
//!
//! Two layers, both offline + deterministic (Tier-0 #557: nothing compiles locally, but these
//! `bash`-invoked pure-function + dry-run composition tests run against the REAL scripts):
//!   1. the PURE classifier `imag_obs_restart_storm_classify` + probe builder in
//!      `scripts/lib/imag-obs-restart-storm.sh` (a time-windowed "N restarts per window_s" rule,
//!      fail-safe = never false-page), and
//!   2. the CALLER GLUE `restart_storm_check` in the watchdog (the enable-flag gate, the reused
//!      `obs_watchdog_alert_throttle` dedup, the dry-run notify path), via the `--dry-run` run with
//!      the ssh probe replaced by a fixture and `IMAG_OBS_RESTART_NOW` driving the window clock.
//!
//! Same fixture-shim / dry-run style as `tests/harness_asio_starve_alert_watchdog_1023.rs`.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const LIB: &str = "scripts/lib/imag-obs-restart-storm.sh";
const WATCHDOG: &str = "scripts/imag-obs-alert-watchdog.sh";

// ── pure-classifier harness ─────────────────────────────────────────────────
// Source the real lib in a fresh bash and call the pure function; capture its stdout. No I/O, no
// ssh, no time dependency (the caller passes `now` explicitly).
fn classify(
    prev_baseline: &str,
    prev_ts: &str,
    cur_probe: &str,
    now: &str,
    threshold: &str,
    window_s: &str,
) -> String {
    let lib = manifest_dir().join(LIB);
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{}'; imag_obs_restart_storm_classify '{}' '{}' '{}' '{}' '{}' '{}'",
            lib.display(),
            prev_baseline,
            prev_ts,
            cur_probe,
            now,
            threshold,
            window_s
        ))
        .output()
        .expect("run classifier");
    assert!(
        out.status.success(),
        "classifier exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn field(out: &str, key: &str) -> String {
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("classifier output missing '{key}=' line in:\n{out}"))
        .to_string()
}

// The very first pass ever (no persisted baseline) only ESTABLISHES the baseline — never a storm.
#[test]
fn first_pass_only_establishes_baseline_never_storms() {
    let out = classify("", "", "NRestarts=100", "1000", "10", "600");
    assert_eq!(field(&out, "storm"), "0", "{out}");
    assert_eq!(field(&out, "baseline"), "100", "{out}");
    assert_eq!(field(&out, "baseline_ts"), "1000", "{out}");
    assert!(field(&out, "reason").contains("first-pass"), "{out}");
}

// The real incident shape: ~17 restarts within one 5-min pass (elapsed 300s <= window 600s,
// delta 17 >= threshold 10) → STORM, and the window re-anchors to the current counter+time.
#[test]
fn delta_over_threshold_within_window_is_a_storm() {
    let out = classify("100", "1000", "NRestarts=117", "1300", "10", "600");
    assert_eq!(field(&out, "storm"), "1", "{out}");
    assert_eq!(field(&out, "baseline"), "117", "{out}");
    assert_eq!(field(&out, "baseline_ts"), "1300", "{out}");
}

// Delta exactly AT the threshold is a storm (>= , not >).
#[test]
fn delta_exactly_at_threshold_is_a_storm() {
    let out = classify("100", "1000", "NRestarts=110", "1300", "10", "600");
    assert_eq!(field(&out, "storm"), "1", "{out}");
}

// A few restarts within the window (below threshold) is NOT a storm, and it KEEPS the original
// window anchor so restarts keep accumulating toward the threshold across sub-window passes.
#[test]
fn below_threshold_within_window_accumulates_keeps_anchor() {
    let out = classify("100", "1000", "NRestarts=103", "1300", "10", "600");
    assert_eq!(field(&out, "storm"), "0", "{out}");
    assert_eq!(field(&out, "baseline"), "100", "{out}");
    assert_eq!(field(&out, "baseline_ts"), "1000", "{out}");
    assert!(field(&out, "reason").contains("accumulating"), "{out}");
}

// Faithful "N per window_s": delta >= threshold but spread over MORE than window_s is a SLOW
// accumulation, not a storm — slide the window (re-anchor), never page.
#[test]
fn delta_over_threshold_but_past_window_is_not_a_storm() {
    let out = classify("100", "1000", "NRestarts=115", "2000", "10", "600");
    assert_eq!(field(&out, "storm"), "0", "{out}");
    assert_eq!(field(&out, "baseline"), "115", "{out}");
    assert_eq!(field(&out, "baseline_ts"), "2000", "{out}");
    assert!(field(&out, "reason").contains("window-expired"), "{out}");
}

// A DECREASING counter means systemd reset it (box reboot / `systemctl reset-failed` / unit
// reinstall) — re-baseline, never page.
#[test]
fn counter_reset_rebaselines_never_storms() {
    let out = classify("100", "1000", "NRestarts=2", "1300", "10", "600");
    assert_eq!(field(&out, "storm"), "0", "{out}");
    assert_eq!(field(&out, "baseline"), "2", "{out}");
    assert_eq!(field(&out, "baseline_ts"), "1300", "{out}");
    assert!(field(&out, "reason").contains("counter-reset"), "{out}");
}

// Fail-safe: an unreadable counter (ssh worked, but the unit query failed) is NEVER a storm and
// must PRESERVE the prior window anchor (so the next readable pass computes the delta correctly).
#[test]
fn unreadable_counter_is_never_a_storm_and_preserves_anchor() {
    let out = classify("100", "1000", "NRESTARTS_QUERY=FAILED", "1300", "10", "600");
    assert_eq!(field(&out, "storm"), "0", "{out}");
    assert_eq!(field(&out, "baseline"), "100", "{out}");
    assert_eq!(field(&out, "baseline_ts"), "1000", "{out}");
    assert!(field(&out, "reason").contains("unreadable"), "{out}");
}

// #1156 review (🔵): a leading-zero counter must be read base-10, never octal -- a bare
// `$(( 08 - 3 ))` aborts the whole pass under the caller's `set -e`. The classifier coerces with 10#.
#[test]
fn leading_zero_counter_is_base10_never_octal_and_never_aborts() {
    // classify() asserts the bash exited 0 -- before the 10# coercion this ABORTED (exit 1) on `08`.
    let out = classify("3", "1000", "NRestarts=08", "1300", "10", "600");
    assert_eq!(field(&out, "storm"), "0", "{out}");
    // 08 read as base-10 8 -> delta 5 (accumulating), not a storm, no abort.
    assert!(field(&out, "reason").contains("delta=5"), "{out}");
}

// ── probe-command builder ───────────────────────────────────────────────────
fn probe_cmd() -> String {
    let lib = manifest_dir().join(LIB);
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{}'; imag_obs_restart_counter_probe_cmd",
            lib.display()
        ))
        .output()
        .expect("run probe builder");
    assert!(out.status.success(), "probe builder exited non-zero");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn probe_cmd_reads_nrestarts_over_the_user_bus() {
    let p = probe_cmd();
    assert!(
        p.contains("XDG_RUNTIME_DIR"),
        "the remote snippet must export XDG_RUNTIME_DIR to reach the user bus over a non-login ssh \
         (issue 998): {p}"
    );
    assert!(
        p.contains("systemctl --user show imag-obs.service") && p.contains("NRestarts"),
        "the remote snippet must read imag-obs.service's NRestarts via `systemctl --user show`: {p}"
    );
    assert!(
        p.contains("NRESTARTS_QUERY=FAILED"),
        "an unreadable counter must print an explicit sentinel, never an empty line: {p}"
    );
}

// ── caller-glue (composition) via the real watchdog, offline + dry-run ──────
// Run the watchdog with the ssh probe replaced by a fixture (IMAG_OBS_RESTART_PROBE_CMD) and the
// window clock driven by IMAG_OBS_RESTART_NOW, so a two-pass storm is deterministic with no real
// time or network. --dry-run never fires a real notify.
struct Rig {
    _dir: tempfile::TempDir,
    state: PathBuf,
    fixture: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        Rig {
            state: dir.path().join("alert.state"),
            fixture: dir.path().join("nrestarts.txt"),
            _dir: dir,
        }
    }

    fn set_counter(&self, probe_line: &str) {
        std::fs::write(&self.fixture, format!("{probe_line}\n")).unwrap();
    }

    // One watchdog pass. `enable`=false leaves IMAG_OBS_RESTART_STORM_ENABLE unset (default OFF).
    // One restart_storm_check pass, run by SOURCING the real watchdog (never executing main(), so
    // no reachability ssh, no network) and calling restart_storm_check directly under the caller's
    // EXACT `set -euo pipefail`. That also proves the check is -e-safe: a bare-statement abort under
    // -e would swallow the log lines this asserts. `enable`=false leaves the flag unset (default OFF).
    fn pass(&self, enable: bool, now: &str) -> String {
        let wd = manifest_dir().join(WATCHDOG);
        let script = format!(
            "set -euo pipefail\nsource '{wd}'\nDRY_RUN=1\nrestart_storm_check\n",
            wd = wd.display()
        );
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(script)
            .env(
                "IMAG_OBS_ALERT_STATE_FILE",
                self.state.display().to_string(),
            )
            .env(
                "IMAG_OBS_RESTART_PROBE_CMD",
                format!("cat {}", self.fixture.display()),
            )
            .env("IMAG_OBS_RESTART_NOW", now)
            .env("IMAG_OBS_RESTART_STORM_THRESHOLD", "10")
            .env("IMAG_OBS_RESTART_STORM_WINDOW_S", "600");
        if enable {
            cmd.env("IMAG_OBS_RESTART_STORM_ENABLE", "1");
        }
        let out = cmd.output().expect("run restart_storm_check");
        // restart_storm_check logs to stderr (via the watchdog's log()).
        String::from_utf8_lossy(&out.stderr).to_string()
    }
}

// Ships DISABLED: with the enable flag unset, the storm check never runs (no page, no ssh).
#[test]
fn restart_storm_ships_disabled_by_default() {
    let rig = Rig::new();
    rig.set_counter("NRestarts=500");
    let log = rig.pass(false, "2000");
    assert!(
        !log.contains("RESTART STORM"),
        "with IMAG_OBS_RESTART_STORM_ENABLE unset the storm detector must never fire: {log}"
    );
    assert!(
        log.contains("restart-storm check disabled"),
        "a disabled pass must say so explicitly: {log}"
    );
}

// Enabled: pass 1 establishes the baseline (no page), pass 2 with +17 restarts inside the window
// fires exactly one dry-run storm page.
#[test]
fn enabled_storm_across_two_passes_pages_once() {
    let rig = Rig::new();

    rig.set_counter("NRestarts=100");
    let p1 = rig.pass(true, "1000");
    assert!(
        !p1.contains("WOULD alert") || !p1.contains("RESTART STORM"),
        "pass 1 only establishes the baseline — no storm page: {p1}"
    );

    rig.set_counter("NRestarts=117");
    let p2 = rig.pass(true, "1300");
    assert!(
        p2.contains("WOULD alert") && p2.contains("RESTART STORM"),
        "pass 2 (+17 restarts in-window) must fire a dry-run storm page: {p2}"
    );
}

// Enabled but the counter is unreadable → nothing to decide, never a page.
#[test]
fn enabled_unreadable_counter_never_pages() {
    let rig = Rig::new();
    rig.set_counter("NRESTARTS_QUERY=FAILED");
    let log = rig.pass(true, "2000");
    assert!(
        !log.contains("WOULD alert"),
        "an unreadable NRestarts counter must never page: {log}"
    );
}

// ── static anchors: the watchdog REUSES the framework, never a second prober ─
#[test]
fn watchdog_sources_the_storm_lib_and_calls_the_check() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("lib/imag-obs-restart-storm.sh"),
        "the watchdog must source the restart-storm lib (#1156)"
    );
    assert!(
        body.contains("restart_storm_check"),
        "the watchdog main pass must call restart_storm_check (#1156)"
    );
}

#[test]
fn restart_storm_reuses_the_shared_throttle_never_a_new_mechanism() {
    let body = read(WATCHDOG);
    // The storm alert must dedup through the SAME #391 pure throttle the down-alert already uses.
    assert!(
        body.contains("obs_watchdog_alert_throttle"),
        "the storm page must reuse obs_watchdog_alert_throttle — never a second dedup mechanism"
    );
    assert!(
        body.contains("imag-restart-storm"),
        "the storm alert must carry a stable dedup signature ('imag-restart-storm') so it pages \
         once per episode + ~1h reminders, not every pass"
    );
}

#[test]
fn restart_storm_ships_disabled_via_env_flag() {
    let body = read(WATCHDOG);
    assert!(
        body.contains("IMAG_OBS_RESTART_STORM_ENABLE"),
        "the storm check must be gated behind IMAG_OBS_RESTART_STORM_ENABLE so it ships DISABLED \
         (enabled by a supervisor Environment= edit to the .service), per the dev1-watchdog \
         convention (#1156)"
    );
}
