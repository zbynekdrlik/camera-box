//! #813 -- measurement A/V-sync LINE GO/NO-GO + stream-state-bound liveness alarm, DEV1-SIDE
//! (scripts/avsync-lineup-alert-watchdog.sh + the pure scripts/avsync_lineup.py decider).
//!
//! Background: the existing scripts/avsync-heartbeat-alert-watchdog.sh alarms on heartbeat
//! STALENESS only, and UNCONDITIONALLY -- so it would NOT have paged the 2026-08-17 silent-audio
//! incident (the heartbeat stayed FRESH; only the CONTENT died -> "measured: unknown, candidates:
//! 0") and a plain stale-log alarm can't tell a legitimately-off box from a dead watchdog during a
//! live event. This watchdog reads the SAME on-box heartbeat + the stream's outputActive and routes
//! the whole judgment through the pure avsync_lineup.py decider, reusing the SAME #391 confirm/
//! throttle lib + airuleset.py notify path (never a second alerting mechanism).
//!
//! These are pure-shell / content tests + behavioral runs with a fake `sshpass` (heartbeat), a fake
//! obs_phase2 (stream state), a fake notify and a fake `systemctl`/`curl` on PATH -- no rig, no real
//! network. `python3` itself is REAL so the behavioral runs exercise the ACTUAL avsync_lineup.py
//! decider end-to-end (the gather -> decide -> alert wiring), mirroring
//! tests/harness_avsync_heartbeat_alert_watchdog.rs's own fake-PATH style for the parts that don't
//! need a live network call.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn script() -> PathBuf {
    manifest_dir().join("scripts/avsync-lineup-alert-watchdog.sh")
}
fn decider() -> PathBuf {
    manifest_dir().join("scripts/avsync_lineup.py")
}
fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SERVICE_UNIT: &str = "systemd/avsync-lineup-alert-watchdog.service";
const TIMER_UNIT: &str = "systemd/avsync-lineup-alert-watchdog.timer";

// ================================================================================================
// Content: reuses the EXISTING #391 decision lib + the #812 heartbeat-parsing lib + the NEW #813
// pure decider + obs_phase2 stream-status -- never a second/third alerting or measurement path.
// ================================================================================================

#[test]
fn watchdog_reuses_the_shared_decision_and_heartbeat_libs() {
    let body = read("scripts/avsync-lineup-alert-watchdog.sh");
    assert!(
        body.contains("lib/obs-watchdog-decision.sh"),
        "must reuse the #391 pure confirm/throttle functions -- never a second mechanism"
    );
    assert!(
        body.contains("lib/avsync-heartbeat.sh"),
        "must reuse the shared heartbeat probe/parse lib"
    );
    assert!(
        body.contains("avsync_lineup.py"),
        "must route the judgment through the pure single-source-of-truth decider"
    );
}

#[test]
fn watchdog_binds_to_stream_state_via_obs_phase2_stream_status() {
    let body = read("scripts/avsync-lineup-alert-watchdog.sh");
    assert!(
        body.contains("stream-status"),
        "the whole point of #813: the alarm is BOUND to the stream's outputActive (read via \
         obs_phase2.py stream-status), not a bare heartbeat-staleness check"
    );
    assert!(
        body.contains("gather_stream_state"),
        "must have a distinct stream-state gather step: {body}"
    );
}

#[test]
fn watchdog_has_both_modes_and_the_shared_notify_path() {
    let body = read("scripts/avsync-lineup-alert-watchdog.sh");
    assert!(
        body.contains("--assert"),
        "must support the one-shot pre-event GO/NO-GO assert mode"
    );
    assert!(
        body.contains("notify --body"),
        "alerts must go through the SAME airuleset.py notify path as the #391/#812 siblings"
    );
}

#[test]
fn systemd_units_wire_the_script_on_a_5min_timer() {
    let service = read(SERVICE_UNIT);
    let timer = read(TIMER_UNIT);
    assert!(
        service.contains("avsync-lineup-alert-watchdog.sh"),
        "the service must ExecStart the watchdog script"
    );
    assert!(
        timer.contains("OnUnitActiveSec=5min"),
        "the timer must fire every 5 min (mirrors the #812 sibling)"
    );
    assert!(
        timer.contains("[Install]") && timer.contains("WantedBy=timers.target"),
        "the timer must be installable"
    );
}

// ================================================================================================
// Behavioral: fake `sshpass` (heartbeat), fake obs_phase2 (stream state), fake notify + fake
// `systemctl`/`curl`, REAL python3 running the REAL avsync_lineup.py.
// ================================================================================================

struct Harness {
    _tmp: tempfile::TempDir,
    fake_bin: tempfile::TempDir,
    state_file: PathBuf,
    notify_marker: PathBuf,
    obs_phase2_fake: PathBuf,
    notify_fake: PathBuf,
    discord_env_file: PathBuf,
}

impl Harness {
    fn new(heartbeat_status: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_bin = tempfile::tempdir().expect("tempdir");
        let state_file = tmp.path().join("state");
        let notify_marker = tmp.path().join("notify-calls.log");
        let obs_phase2_fake = tmp.path().join("fake_obs_phase2.py");
        let notify_fake = tmp.path().join("fake_notify.py");
        let discord_env_file = tmp.path().join("discord.env");

        // fake sshpass on PATH: emit the REAL cmd.exe-shaped heartbeat (CRLF + the trailing-space
        // separator, byte-matching the live probe) with a FRESH epoch computed at runtime, plus an
        // empty vlc segment. `avsync_heartbeat_probe_cmd` builds the remote command; the fake
        // ignores it and prints the fixture heartbeat.
        write_exec(
            &fake_bin.path().join("sshpass"),
            &format!(
                "#!/bin/sh\nprintf '%s\\t%s\\r\\n' \"$(date +%s)\" \"{heartbeat_status}\"\n\
                 printf '%s\\r\\n' '---AVSYNC-HB-SEP--- '\nprintf '\\r\\n'\nexit 0\n"
            ),
        );
        // fake systemctl: exits per FAKE_SYSTEMCTL_RC (0 = unit active). One stub covers both the
        // forwarder-active (GO) and forwarder-down (NO-GO) preflight cases.
        write_exec(
            &fake_bin.path().join("systemctl"),
            "#!/bin/sh\nexit ${FAKE_SYSTEMCTL_RC:-0}\n",
        );
        // fake curl: mimics `curl -w '\\n%{http_code}'` (body, newline, code) for the Discord
        // test-ping, code overridable via FAKE_CURL_HTTP_CODE.
        write_exec(
            &fake_bin.path().join("curl"),
            "#!/bin/sh\nprintf '%s\\n%s' '{}' \"${FAKE_CURL_HTTP_CODE:-200}\"\nexit 0\n",
        );

        // fake obs_phase2 target (run by REAL python3): prints active=<FAKE_STREAM_ACTIVE>, ignoring
        // argv -- so one file covers active/inactive/unknown.
        fs::write(
            &obs_phase2_fake,
            "import os\nprint('active=' + os.environ.get('FAKE_STREAM_ACTIVE', 'True') + ' path=')\n",
        )
        .expect("write fake obs_phase2");
        // fake notify target (run by REAL python3): logs its argv to the marker.
        fs::write(
            &notify_fake,
            format!(
                "import sys\nopen(r'{}', 'a').write('CALLED: ' + ' '.join(sys.argv[1:]) + '\\n')\n",
                notify_marker.display()
            ),
        )
        .expect("write fake notify");
        fs::write(
            &discord_env_file,
            "DISCORD_BOT_TOKEN=fake-test-token\nDISCORD_MENTION_ZBYNEK=123456789012345678\n",
        )
        .expect("write discord env fixture");

        Harness {
            _tmp: tmp,
            fake_bin,
            state_file,
            notify_marker,
            obs_phase2_fake,
            notify_fake,
            discord_env_file,
        }
    }

    fn run(&self, body: &str, extra_env: &[(&str, &str)]) -> (i32, String, String) {
        let path = format!(
            "{}:{}",
            self.fake_bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(body)
            .env("SCRIPT", script())
            .env("AVSYNC_LINEUP_DECIDER", decider())
            .env("AVSYNC_LINEUP_OBS_PHASE2", &self.obs_phase2_fake)
            .env("AIRULESET_NOTIFY", &self.notify_fake)
            .env("AVSYNC_LINEUP_STATE_FILE", &self.state_file)
            .env("AVSYNC_DISCORD_ENV", &self.discord_env_file)
            .env("PATH", path);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run bash harness");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn notify_calls(&self) -> String {
        fs::read_to_string(&self.notify_marker).unwrap_or_default()
    }
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    let mut perm = fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(path, perm).unwrap();
}

// --- the load-bearing case: TODAY (2026-08-17) would have paged --------------------------------

#[test]
fn liveness_alarms_when_stream_live_and_content_silent_the_2026_08_17_case() {
    // heartbeat FRESH but status = silent/undecodable content on a successful grab; stream LIVE.
    let h = Harness::new(r#"measured: av_sync verdict: \"unknown\", candidates: 0"#);
    let (_code, _out, err) = h.run(
        ". \"$SCRIPT\"\nDRY_RUN=1\nmain",
        &[("FAKE_STREAM_ACTIVE", "True")],
    );
    assert!(
        err.contains("action=ALARM"),
        "a fresh heartbeat with unknown/candidates:0 content DURING a live stream must be ALARM \
         (the staleness-only watchdog misses this): stderr={err}"
    );
    assert!(
        err.contains("WOULD alert") && err.contains("ZIVEHO streamu"),
        "must reach the (dry-run) alert branch with the stream-live message: stderr={err}"
    );
}

#[test]
fn liveness_alarm_fires_the_notify_when_not_dry_run() {
    let h = Harness::new(r#"measured: av_sync verdict: \"unknown\", candidates: 0"#);
    let (_c, _o, _e) = h.run(". \"$SCRIPT\"\nmain", &[("FAKE_STREAM_ACTIVE", "True")]);
    let calls = h.notify_calls();
    assert!(
        calls.contains("notify") && calls.contains("MRTVA"),
        "a confirmed ALARM must actually invoke airuleset.py notify: calls={calls:?}"
    );
}

#[test]
fn liveness_suppressed_when_stream_off_air_even_with_a_dead_line() {
    // stream not emitting -> a dead line is EXPECTED (box off / between events) -> never page.
    let h = Harness::new("no-signal: relay down");
    let (_c, _o, err) = h.run(
        ". \"$SCRIPT\"\nDRY_RUN=1\nmain",
        &[("FAKE_STREAM_ACTIVE", "False")],
    );
    assert!(
        err.contains("action=SUPPRESSED"),
        "an off-air stream must SUPPRESS even a dead line: stderr={err}"
    );
    assert!(
        !err.contains("WOULD alert"),
        "SUPPRESSED must never reach the alert branch: stderr={err}"
    );
}

#[test]
fn liveness_ok_when_stream_live_and_line_healthy() {
    let h = Harness::new("measured: A/V sync OK (offset 0 ms)");
    let (_c, _o, err) = h.run(
        ". \"$SCRIPT\"\nDRY_RUN=1\nmain",
        &[("FAKE_STREAM_ACTIVE", "True")],
    );
    assert!(err.contains("action=OK"), "stderr={err}");
    assert!(
        !err.contains("WOULD alert"),
        "OK must not alert: stderr={err}"
    );
}

// --- pre-event GO/NO-GO assert -----------------------------------------------------------------

#[test]
fn assert_go_when_everything_green() {
    // fresh healthy heartbeat + forwarder active (systemctl rc 0) + Discord test-ping 200.
    let h = Harness::new("measured: A/V sync OK (offset 0 ms)");
    let (code, out, _e) = h.run(
        ". \"$SCRIPT\"\nMODE=assert\nmain",
        &[("FAKE_SYSTEMCTL_RC", "0"), ("FAKE_CURL_HTTP_CODE", "200")],
    );
    assert_eq!(
        code, 0,
        "all-green preflight must exit 0 (GO): stdout={out}"
    );
    assert!(out.contains("GO") && !out.contains("NO-GO"), "stdout={out}");
}

#[test]
fn assert_no_go_and_alerts_when_forwarder_down() {
    // forwarder timer inactive (systemctl rc 1) -> NO-GO -> exit 1 + a loud pre-event alert.
    let h = Harness::new("measured: A/V sync OK (offset 0 ms)");
    let (code, out, _e) = h.run(
        ". \"$SCRIPT\"\nMODE=assert\nmain",
        &[("FAKE_SYSTEMCTL_RC", "1"), ("FAKE_CURL_HTTP_CODE", "200")],
    );
    assert_eq!(
        code, 1,
        "a dead forwarder must be NO-GO (exit 1): stdout={out}"
    );
    assert!(out.contains("NO-GO"), "stdout={out}");
    let calls = h.notify_calls();
    assert!(
        calls.contains("PRE-EVENT NO-GO"),
        "a NO-GO must fire a loud pre-event alert: calls={calls:?}"
    );
}

#[test]
fn assert_no_go_when_discord_ping_not_delivered() {
    // forwarder active but the Discord test-ping returns 403 -> not delivered -> NO-GO.
    let h = Harness::new("measured: A/V sync OK (offset 0 ms)");
    let (code, out, _e) = h.run(
        ". \"$SCRIPT\"\nMODE=assert\nmain",
        &[("FAKE_SYSTEMCTL_RC", "0"), ("FAKE_CURL_HTTP_CODE", "403")],
    );
    assert_eq!(
        code, 1,
        "an undelivered test-ping must be NO-GO: stdout={out}"
    );
    assert!(out.contains("NO-GO"), "stdout={out}");
}
