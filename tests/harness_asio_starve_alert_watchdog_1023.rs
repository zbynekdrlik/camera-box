//! #1023 — offline integration guard for `scripts/asio-starve-alert-watchdog.sh`'s DECISION
//! COMPOSITION (`main`/`handle_source`): the two-phase healthy-sibling computation, the STARVED
//! 2-pass confirm → alert, the box-wide all-starving → UNKNOWN (never a page), the box-down → SKIP
//! (#1001 no-double-page), and the recovery ping. The pure decision core is covered exhaustively by
//! `tests/harness_asio_starve_health_1023.rs`; this file covers the CALLER GLUE that actually drives
//! the alert/recovery side effects and the cross-source `others_healthy` arithmetic, so a wiring /
//! discriminator regression is caught locally (per #414 — an unattended production timer's novel
//! logic must be tested, weighted like a correctness bug).
//!
//! Fully offline + deterministic: the ssh probe is replaced via `ASIO_STARVE_PROBE_CMD` (so
//! `require_tools` needs no sshpass/ssh/timeout), the run is `--dry-run` (never a real notify), and
//! the state files are per-test tempdir paths (`.claude/rules/ci-testing-gotchas.md` #975 tempdir
//! isolation). The probe cats a fixture file the test rewrites per pass (#836 per-pass fixture). Same
//! fixture-shim pattern as `tests/harness_bundle_state_watchdog_732.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/asio-starve-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

// A stream-OBS asrc log line for one source at one interval.
fn asrc_line(ts: &str, source: &str, blocks: u32) -> String {
    format!(
        "{ts}: asrc: source '{source}' estimated=0.00ppm applied=0.00ppm outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks={blocks} (#803/#806/#960)\n"
    )
}

/// A per-test rig: a tempdir with a probe.sh that cats a fixture log the test rewrites per pass,
/// plus the alert + network-reach state files. Reused across passes within one test so the confirm /
/// alert / recovery latches persist exactly as they would on the real 5-min timer.
struct Rig {
    _dir: tempfile::TempDir,
    probe: PathBuf,
    logfix: PathBuf,
    state: PathBuf,
    netreach: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe.sh");
        // The probe ignores its <box_ip> arg and prints the current fixture log (ASIO_STARVE_TEST_LOG
        // is inherited from the watchdog's environment, which inherits it from this test's Command).
        write_exec(
            &probe,
            "#!/usr/bin/env bash\ncat \"$ASIO_STARVE_TEST_LOG\" 2>/dev/null || true\n",
        );
        Rig {
            probe,
            logfix: dir.path().join("obslog.txt"),
            state: dir.path().join("asio-starve.state"),
            netreach: dir.path().join("netreach.state"),
            _dir: dir,
        }
    }

    /// Seed issue-1001's state so the stream box reads CONFIRMED-unreachable (its page is #1001's).
    fn seed_box_down(&self) {
        fs::write(&self.netreach, "alerted_stream=1\n").unwrap();
    }

    /// Run ONE `--dry-run` pass with `log` as the stream OBS-log tail. Returns stdout+stderr (the
    /// watchdog logs to stderr).
    fn pass(&self, log: &str) -> String {
        fs::write(&self.logfix, log).unwrap();
        let out = Command::new("bash")
            .arg(watchdog())
            .arg("--dry-run")
            .env(
                "ASIO_STARVE_PROBE_CMD",
                format!("bash {}", self.probe.display()),
            )
            .env("ASIO_STARVE_TEST_LOG", &self.logfix)
            .env("ASIO_STARVE_ALERT_STATE_FILE", &self.state)
            .env("ASIO_STARVE_NETREACH_STATE_FILE", &self.netreach)
            .env("ASIO_STARVE_SOURCES", "ASIO Input Capture;mbc")
            .env("ASIO_STARVE_THRESHOLD", "1000")
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run watchdog");
        assert!(
            out.status.success(),
            "watchdog pass exited non-zero: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }
}

// 'ASIO Input Capture' starving (~2946) while the sibling 'mbc' is healthy (0) — the #1023 defect.
fn starved_log() -> String {
    asrc_line("09:00:07.551", "ASIO Input Capture", 2949) + &asrc_line("09:00:08.660", "mbc", 0)
}
// Both real sources starving — a box-wide audio outage (no healthy sibling).
fn both_starved_log() -> String {
    asrc_line("09:00:07.551", "ASIO Input Capture", 2949) + &asrc_line("09:00:08.660", "mbc", 2900)
}
// Both healthy — receiving audio.
fn healthy_log() -> String {
    asrc_line("09:00:07.551", "ASIO Input Capture", 0) + &asrc_line("09:00:08.660", "mbc", 0)
}

// ---------------------------------------------------------------------------------------------
// (a) starved + healthy sibling: pass 1 HOLDS (2-pass confirm), pass 2 ALERTS.
// ---------------------------------------------------------------------------------------------
#[test]
fn starved_with_healthy_sibling_holds_one_pass_then_alerts() {
    let rig = Rig::new();
    let p1 = rig.pass(&starved_log());
    assert!(
        p1.contains("-> STARVED"),
        "pass1 should classify STARVED: {p1}"
    );
    assert!(
        p1.contains("holding") && !p1.contains("WOULD alert"),
        "pass1 must HOLD (2-pass confirm), not alert: {p1}"
    );
    // The healthy sibling is not an incident.
    assert!(
        p1.contains("'mbc' on stream") && p1.contains("-> OK"),
        "pass1 should read 'mbc' OK: {p1}"
    );

    let p2 = rig.pass(&starved_log());
    assert!(
        p2.contains("WOULD alert: 'ASIO Input Capture' CONFIRMED starved"),
        "pass2 must alert once CONFIRMED across 2 passes: {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) box-wide (every watched source starving, no healthy sibling): UNKNOWN, NEVER pages.
// ---------------------------------------------------------------------------------------------
#[test]
fn box_wide_all_starving_stays_unknown_and_never_pages() {
    let rig = Rig::new();
    let p1 = rig.pass(&both_starved_log());
    assert!(
        p1.contains("-> UNKNOWN") && !p1.contains("WOULD alert"),
        "all-starving pass1 must be UNKNOWN, never a page (obs-liveness/audio-presence own a box-wide outage): {p1}"
    );
    // A second pass must STILL never page — UNKNOWN resets the confirm streak.
    let p2 = rig.pass(&both_starved_log());
    assert!(
        !p2.contains("WOULD alert"),
        "all-starving must never page even sustained: {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// (c) box down per #1001: SKIP every source, never a double page.
// ---------------------------------------------------------------------------------------------
#[test]
fn box_down_per_issue_1001_skips_all_sources() {
    let rig = Rig::new();
    rig.seed_box_down();
    let p = rig.pass(&starved_log());
    assert!(
        p.contains("SKIP all sources this pass") && !p.contains("WOULD alert"),
        "a #1001-confirmed-down box must SKIP (no double page): {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// (d) recovery: after a source we PAGED for reads OK again, fire ONE recovery ping.
// ---------------------------------------------------------------------------------------------
#[test]
fn recovery_ping_fires_once_after_a_paged_source_clears() {
    let rig = Rig::new();
    rig.pass(&starved_log()); // pass1 holds
    let p2 = rig.pass(&starved_log()); // pass2 alerts (latches alerted)
    assert!(
        p2.contains("WOULD alert"),
        "precondition: pass2 alerts: {p2}"
    );

    let p3 = rig.pass(&healthy_log());
    assert!(
        p3.contains("WOULD send recovery: 'ASIO Input Capture'"),
        "a paged source reading OK again must fire ONE recovery ping: {p3}"
    );
    // Recovery is one-shot: a second healthy pass must not re-send it.
    let p4 = rig.pass(&healthy_log());
    assert!(
        !p4.contains("WOULD send recovery"),
        "recovery must fire only once: {p4}"
    );
}
