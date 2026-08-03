//! #812/#807 — stream-box avsync heartbeat alert, DEV1-SIDE (scripts/avsync-heartbeat-alert-watchdog.sh).
//!
//! Background: neither avsync-watchdog.ps1 (#812) nor avsync-vlc-monitor.ps1 (#807) can alert
//! Discord on their OWN silence, and the stream box has no ~/devel/airuleset checkout / Discord
//! credentials of its own (the same topology gap #882's imag-obs-alert-watchdog.sh and #391's
//! obs-liveness-watchdog.sh already close for their own boxes). This moves the alert to DEV1,
//! reusing the EXISTING pure scripts/lib/obs-watchdog-decision.sh confirm/throttle functions, with
//! TWO independent legs ("watchdog" and "vlc") read in a single ssh round-trip.
//!
//! Pure-shell / content tests -- no rig, no real ssh (measure() is exercised via a fake
//! `sshpass`/`ssh` on PATH returning a fixed combined probe response, mirroring
//! tests/harness_imag_obs_alert_watchdog_882.rs's own test style for the parts that don't need a
//! live network call).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/avsync-heartbeat-alert-watchdog.sh")
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/avsync-heartbeat.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SERVICE_UNIT: &str = "systemd/avsync-heartbeat-alert-watchdog.service";
const TIMER_UNIT: &str = "systemd/avsync-heartbeat-alert-watchdog.timer";

// ================================================================================================
// Content: reuses the EXISTING #391 decision lib + the new #812/#807 heartbeat-parsing lib, never
// a third alerting mechanism.
// ================================================================================================

#[test]
fn watchdog_sources_the_shared_decision_lib_never_a_third_mechanism() {
    let body = read("scripts/avsync-heartbeat-alert-watchdog.sh");
    assert!(
        body.contains("lib/obs-watchdog-decision.sh"),
        "must reuse the #391 pure confirm/throttle functions -- never invent a second/third mechanism"
    );
    assert!(
        body.contains("lib/avsync-heartbeat.sh"),
        "must reuse the shared heartbeat-parsing lib"
    );
    assert!(
        body.contains("process_leg") && body.contains("\"watchdog\"") && body.contains("\"vlc\""),
        "must process BOTH legs independently (own confirm/throttle state each): {body}"
    );
}

#[test]
fn watchdog_fires_through_the_same_airuleset_notify_path_as_391_and_882() {
    let body = read("scripts/avsync-heartbeat-alert-watchdog.sh");
    assert!(body.contains("airuleset.py") && body.contains("notify --body"));
}

// ================================================================================================
// scripts/lib/avsync-heartbeat.sh -- pure parsing/staleness functions, tested directly.
// ================================================================================================

fn run_lib(body: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$LIB\"\nset +e\n{body}"))
        .env("LIB", lib_script())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn probe_cmd_uses_type_with_a_separator_and_never_fails_on_a_missing_file() {
    let (code, out, err) = run_lib("avsync_heartbeat_probe_cmd");
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("type ") && out.contains("2>nul"),
        "must use cmd.exe `type`, swallowing a missing-file error: {out}"
    );
    assert!(
        out.contains("---AVSYNC-HB-SEP---"),
        "must embed the separator: {out}"
    );
    assert!(
        out.contains(" & echo ") && !out.contains(" && echo "),
        "must chain with `&` (always runs the next segment) not `&&` (would skip it on a missing first file): {out}"
    );
}

#[test]
fn extract_segment_splits_on_the_separator_both_directions() {
    let combined = "111\told\n---AVSYNC-HB-SEP---\n222\tvlc-old\n";
    let (code, out, err) = run_lib(&format!(
        "avsync_heartbeat_extract_segment '{combined}' watchdog"
    ));
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("111") && !out.contains("222"),
        "watchdog segment: {out}"
    );

    let (code2, out2, err2) = run_lib(&format!(
        "avsync_heartbeat_extract_segment '{combined}' vlc"
    ));
    assert_eq!(code2, 0, "stderr={err2}");
    assert!(
        out2.contains("222") && !out2.contains("111"),
        "vlc segment: {out2}"
    );
}

#[test]
fn extract_segment_tolerates_real_cmdexe_crlf_and_trailing_space_before_separator_968() {
    // #968: cmd.exe's `echo TEXT & next` emits the separator with a TRAILING SPACE, and every
    // line arrives CRLF-terminated over ssh -- this fixture byte-matches the REAL captured probe
    // output from the live incident (2026-08-03 19:40). Pre-968, the exact-line sed anchors never
    // matched this shape: the vlc leg read permanently empty (<none>), producing a false
    // CONFIRMED-stale alert every ~1h even though the on-box heartbeat file was genuinely fresh.
    let combined = "1785778776\tno-signal: clip too small (94869 B)\r\n---AVSYNC-HB-SEP--- \r\n1785778799\tok, audio-device-missing\r\n";

    let (code, out, err) = run_lib(&format!(
        "avsync_heartbeat_extract_segment '{combined}' watchdog"
    ));
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("1785778776") && !out.contains("1785778799"),
        "watchdog segment must extract its own epoch and never leak into the vlc side: {out:?}"
    );

    let (code2, out2, err2) = run_lib(&format!(
        "avsync_heartbeat_extract_segment '{combined}' vlc"
    ));
    assert_eq!(code2, 0, "stderr={err2}");
    assert!(
        out2.contains("1785778799") && !out2.contains("1785778776"),
        "vlc segment must extract its own epoch -- pre-968 this came back EMPTY (permanently \
         <none>) because the CRLF+trailing-space separator never matched the exact-line anchor: {out2:?}"
    );

    // Prove the whole pipeline, not just the split: the extracted vlc segment must parse cleanly
    // through avsync_heartbeat_last_epoch to the REAL fresh epoch, never empty.
    let (code3, out3, err3) = run_lib(&format!(
        "avsync_heartbeat_last_epoch \"$(avsync_heartbeat_extract_segment '{combined}' vlc)\""
    ));
    assert_eq!(code3, 0, "stderr={err3}");
    assert_eq!(
        out3.trim(),
        "1785778799",
        "the vlc leg's last_epoch must read the REAL fresh epoch, never come back empty/<none>"
    );
}

#[test]
fn extract_segment_with_no_separator_fails_safe_symmetrically_on_both_sides() {
    // Review-caught bug: without the separator (a truncated/partial ssh read), the "vlc" branch
    // correctly returned empty (its own sed range never matches, so it prints nothing) but the
    // "watchdog" branch's range (`1,/^SEP$/p`) silently fell back to "print everything up to
    // EOF, then drop the last line" -- discarding possibly-fresh content instead of failing safe
    // like its sibling. Both sides must come back EMPTY when the separator is genuinely absent.
    let no_separator = "100\tsomething-with-no-separator-at-all\n";
    let (code, out, err) = run_lib(&format!(
        "avsync_heartbeat_extract_segment '{no_separator}' watchdog"
    ));
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.trim().is_empty(),
        "watchdog side must be empty when the separator never appears, got: {out:?}"
    );

    let (code2, out2, err2) = run_lib(&format!(
        "avsync_heartbeat_extract_segment '{no_separator}' vlc"
    ));
    assert_eq!(code2, 0, "stderr={err2}");
    assert!(
        out2.trim().is_empty(),
        "vlc side must be empty when the separator never appears, got: {out2:?}"
    );
}

#[test]
fn last_epoch_picks_the_last_numeric_line_ignoring_noise() {
    let segment = "garbage\n100\tok\n200\tok2\n";
    let (code, out, err) = run_lib(&format!("avsync_heartbeat_last_epoch '{segment}'"));
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "200");
}

#[test]
fn last_epoch_is_empty_when_nothing_parses() {
    let (code, out, err) = run_lib("avsync_heartbeat_last_epoch ''");
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(out.trim(), "");
}

#[test]
fn is_stale_true_for_missing_or_unparseable_never_defaults_to_fresh() {
    for args in ["'' 1000 300", "abc 1000 300", "100 abc 300"] {
        let (code, _out, err) = run_lib(&format!("avsync_heartbeat_is_stale {args}"));
        assert_eq!(
            code, 0,
            "missing/corrupt input must be STALE (exit 0), args='{args}' stderr={err}"
        );
    }
}

#[test]
fn is_stale_false_within_window_true_beyond_it() {
    let (code_fresh, _o, e1) = run_lib("avsync_heartbeat_is_stale 1000 1100 300");
    assert_eq!(
        code_fresh, 1,
        "age=100s within a 300s window must be FRESH: {e1}"
    );
    let (code_stale, _o2, e2) = run_lib("avsync_heartbeat_is_stale 1000 2000 300");
    assert_eq!(
        code_stale, 0,
        "age=1000s beyond a 300s window must be STALE: {e2}"
    );
}

#[test]
fn is_stale_empty_epoch_is_stale_even_when_the_arithmetic_would_otherwise_read_fresh() {
    // Review-caught bug: the original implementation string-concatenated all three args before
    // checking "all digits", so an EMPTY $epoch simply vanished from the concatenation instead of
    // producing a non-digit/empty result -- it fell through to `age = now - epoch` where bash
    // arithmetic silently treats an empty operand as 0. With a `now` SMALLER than `stale` (unlike
    // the pre-existing `is_stale_true_for_missing_or_unparseable...` test's `now=1000, stale=300`,
    // where age=1000 happens to exceed stale=300 and masks the bug by coincidence), the computed
    // age reads well within the window and the function wrongly returns FRESH (exit 1).
    let (code, _out, err) = run_lib("avsync_heartbeat_is_stale '' 100 300");
    assert_eq!(
        code, 0,
        "an empty epoch must be STALE regardless of how the arithmetic would otherwise read: {err}"
    );
}

// ================================================================================================
// Behavioral: run main() with a stubbed `sshpass` returning a fixed combined probe response, and a
// fake `python3` standing in for the real notify call.
// ================================================================================================

fn fake_bin_dir(
    watchdog_body: &str,
    vlc_body: &str,
    notify_marker: &std::path::Path,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let sshpass = dir.path().join("sshpass");
    fs::write(
        &sshpass,
        // #968: emit the REAL cmd.exe-shaped output -- CRLF line endings, and a trailing space
        // before the separator (byte-matching the live captured probe: "...SEP--- \r\n"). The
        // pre-968 stub emitted clean LF-only output with no trailing space and never exercised
        // the actual defect (extract_segment's exact-line sed anchors silently never matching).
        format!(
            "#!/bin/sh\nprintf '%s\\r\\n' \"{watchdog_body}\"\nprintf '%s\\r\\n' '---AVSYNC-HB-SEP--- '\nprintf '%s\\r\\n' \"{vlc_body}\"\nexit 0\n"
        ),
    )
    .expect("write sshpass");
    let mut perm = fs::metadata(&sshpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&sshpass, perm).unwrap();

    let python3 = dir.path().join("python3");
    fs::write(
        &python3,
        format!(
            "#!/bin/sh\necho \"CALLED: $*\" >> {}\nexit 0\n",
            notify_marker.display()
        ),
    )
    .expect("write python3 stub");
    let mut perm2 = fs::metadata(&python3).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm2, 0o755);
    fs::set_permissions(&python3, perm2).unwrap();

    dir
}

struct Harness {
    _tmp: tempfile::TempDir,
    state_file: PathBuf,
    marker_file: PathBuf,
    fake_bin: tempfile::TempDir,
}

impl Harness {
    fn new(watchdog_body: &str, vlc_body: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_file = tmp.path().join("state");
        let marker_file = tmp.path().join("notify-calls.log");
        let fake_bin = fake_bin_dir(watchdog_body, vlc_body, &marker_file);
        Harness {
            _tmp: tmp,
            state_file,
            marker_file,
            fake_bin,
        }
    }

    fn run_main(&self) -> (i32, String, String) {
        let path = format!(
            "{}:{}",
            self.fake_bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$SCRIPT\"\nmain")
            .env("SCRIPT", script())
            .env("AVSYNC_HEARTBEAT_STATE_FILE", &self.state_file)
            .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
            .env("PATH", path)
            .output()
            .expect("run bash harness");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn notify_call_count(&self) -> usize {
        fs::read_to_string(&self.marker_file)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

fn fresh_body(status: &str) -> String {
    // Embeds a literal shell command substitution -- evaluated by the FAKE ssh stub at call time,
    // so the epoch is always genuinely "now" regardless of when the test itself runs. The `\t`
    // here is a REAL tab byte (single backslash in the Rust literal) -- it must survive unchanged
    // through the stub's `printf '%s\n' "..."` (a %s ARGUMENT is never escape-processed by
    // printf, unlike the format string itself), so a literal two-char "\t" would NOT become a
    // real tab and would break avsync_heartbeat_last_epoch's `awk -F'\t'` field split.
    format!("$(date +%s)\t{status}")
}

const STALE_BODY: &str = "1000000000\tstale-old";

#[test]
fn both_legs_fresh_never_alerts() {
    let h = Harness::new(&fresh_body("ok"), &fresh_body("ok"));
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(h.notify_call_count(), 0, "both legs fresh must never alert");
}

#[test]
fn watchdog_leg_stale_alerts_on_first_pass_vlc_stays_quiet() {
    let h = Harness::new(STALE_BODY, &fresh_body("ok"));
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "confirm threshold 1 means the watchdog leg alerts immediately; the vlc leg is fresh and must stay silent"
    );
}

#[test]
fn vlc_leg_stale_alerts_independently_of_watchdog_leg() {
    let h = Harness::new(&fresh_body("ok"), STALE_BODY);
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "the vlc leg's own staleness must alert on its own"
    );
}

#[test]
fn both_legs_stale_alerts_twice_independently() {
    let h = Harness::new(STALE_BODY, STALE_BODY);
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        2,
        "each stale leg gets its own alert in the same pass"
    );
}

#[test]
fn repeated_stale_passes_are_throttled_per_leg() {
    let h = Harness::new(STALE_BODY, &fresh_body("ok"));
    for _ in 0..4 {
        h.run_main();
    }
    assert_eq!(
        h.notify_call_count(),
        1,
        "4 consecutive stale passes with a large throttle window must alert only ONCE for the watchdog leg"
    );
}

#[test]
fn recovery_then_a_new_outage_alerts_again() {
    let h = Harness::new(STALE_BODY, &fresh_body("ok"));
    h.run_main();
    assert_eq!(h.notify_call_count(), 1);

    let h2 = Harness::new(&fresh_body("ok"), &fresh_body("ok"));
    std::fs::copy(&h.state_file, &h2.state_file).ok();
    h2.run_main();
    assert_eq!(h2.notify_call_count(), 0, "a healthy pass must never alert");

    let h3 = Harness::new(STALE_BODY, &fresh_body("ok"));
    std::fs::copy(&h2.state_file, &h3.state_file).ok();
    h3.run_main();
    assert_eq!(
        h3.notify_call_count(),
        1,
        "a NEW outage after recovery must alert again"
    );
}

#[test]
fn dry_run_never_calls_notify() {
    let h = Harness::new(STALE_BODY, STALE_BODY);
    let path = format!(
        "{}:{}",
        h.fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$SCRIPT\"\nDRY_RUN=1\nmain")
        .env("SCRIPT", script())
        .env("AVSYNC_HEARTBEAT_STATE_FILE", &h.state_file)
        .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
        .env("PATH", path)
        .output()
        .expect("run bash harness");
    assert!(out.status.success());
    assert_eq!(
        h.notify_call_count(),
        0,
        "DRY_RUN=1 must never fire a real notify call"
    );
}

#[test]
fn empty_probe_output_ssh_failure_never_falsely_alerts() {
    // A total ssh/connectivity failure (sshpass itself exits non-zero, no stdout at all) must
    // leave PROBE_OUT empty, which main() treats as "nothing to decide this pass" -- never a
    // false "both legs stale" alert. Uses its OWN dedicated fake-bin dir (sshpass fails, python3
    // still stubbed) rather than reusing Harness::new's stub, so the marker file checked below is
    // guaranteed to be the one this exact run's PATH actually points to.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_file = tmp.path().join("state");
    let marker_file = tmp.path().join("notify-calls.log");

    let dir = tempfile::tempdir().expect("tempdir");
    let sshpass = dir.path().join("sshpass");
    fs::write(&sshpass, "#!/bin/sh\nexit 1\n").expect("write sshpass");
    let mut perm = fs::metadata(&sshpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&sshpass, perm).unwrap();
    let python3 = dir.path().join("python3");
    fs::write(
        &python3,
        format!(
            "#!/bin/sh\necho \"CALLED: $*\" >> {}\nexit 0\n",
            marker_file.display()
        ),
    )
    .expect("write python3 stub");
    let mut perm2 = fs::metadata(&python3).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm2, 0o755);
    fs::set_permissions(&python3, perm2).unwrap();

    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$SCRIPT\"\nmain")
        .env("SCRIPT", script())
        .env("AVSYNC_HEARTBEAT_STATE_FILE", &state_file)
        .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
        .env("PATH", path)
        .output()
        .expect("run bash harness");
    assert!(out.status.success());
    let calls = fs::read_to_string(&marker_file)
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(
        calls, 0,
        "an ssh failure must never be read as a false stale alert"
    );
}

// ================================================================================================
// systemd/avsync-heartbeat-alert-watchdog.{service,timer} -- dev1-side unit files.
// ================================================================================================

#[test]
fn unit_files_exist_and_are_wired_correctly() {
    let service = read(SERVICE_UNIT);
    let timer = read(TIMER_UNIT);
    assert!(
        service.contains("avsync-heartbeat-alert-watchdog.sh"),
        "the service unit must ExecStart the watchdog script"
    );
    assert!(
        timer.contains("OnUnitActiveSec=5min"),
        "the timer must fire every 5 min, matching the #882 sibling's own cadence"
    );
    assert!(
        timer.contains("[Install]") && timer.contains("WantedBy=timers.target"),
        "the timer must be installable"
    );
}
