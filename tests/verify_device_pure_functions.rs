//! #454 — pure-function guard for `scripts/verify-device.sh`, the POST-REBOOT runtime acceptance
//! gate for a freshly-provisioned camera-box appliance.
//!
//! Distinct from `tests/setup_device_provisioner_hardening.rs` (which pins `setup-device.sh`
//! STEP 19's INSTALL-TIME, pre-reboot file-presence check): this file exercises
//! `verify-device.sh`'s own pure decision functions, which read REAL post-reboot signals
//! (systemd state, journald, `ls -la`, `avahi-browse`) gathered over SSH by the (untestable-here)
//! live flow. Every pure function is sourced + called directly — same convention as
//! `tests/setup_device_pure_functions.rs` / `tests/clock_offset_guard.rs`.
//!
//! `verify-device.sh` REUSES rather than reinvents:
//!
//! - `scripts/lib/ndi-alive.sh`: `emit_ok_grep_pattern()` / `fatal_grep_pattern()`
//! - `scripts/lib/timesync-authority.sh`: `dpkg_status_installed()` / `timesync_daemon_verdict()` /
//!   `timesync_authority_verdict()` (#591; extracted here in #596 so `scripts/drift-guard.sh`'s
//!   `--check-imag` facet can share the SAME sole-timesync-authority verdict for imag-nb)
//! - `scripts/clock-offset-guard.sh`: `offset_us_from_journal()` / `offset_check()` /
//!   `ptp_locked_from_journal()`
//! - `scripts/camera-set.sh`: `camera_resolve()` (NAME -> IP / `CAMERA_GENLOCK_FPS`)
//!
//! so this file also proves the composition (`dantesync_locked_ok` / `dantesync_offset_verdict` /
//! `ndi_emit_ok` / `ndi_journal_has_fatal`) works against real fixture text, not just that the
//! new script's OWN functions are correct in isolation.
//!
//! RED before `scripts/verify-device.sh` exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/verify-device.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the live SSH flow) and run `body`
/// against its pure functions. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
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
// (a) version format / match
// ---------------------------------------------------------------------------------------------

#[test]
fn version_is_valid_format_accepts_dev_and_release_forms() {
    for v in ["1.7.0-dev.244", "1.7.0", "1.8.16"] {
        let (code, out, err) = run_sourced(&format!(
            r#"if version_is_valid_format "{v}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "harness itself must not crash. stderr: {err}");
        assert_eq!(
            out.trim(),
            "YES",
            "version_is_valid_format('{v}') should accept it"
        );
    }
}

#[test]
fn version_is_valid_format_rejects_garbage() {
    for v in ["", "unknown", "v1.7.0", "1.7-dev.244", "1.7.0-devX.1"] {
        let (code, out, err) = run_sourced(&format!(
            r#"if version_is_valid_format "{v}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "harness itself must not crash. stderr: {err}");
        assert_eq!(
            out.trim(),
            "NO",
            "version_is_valid_format('{v}') should reject it"
        );
    }
}

#[test]
fn version_matches_expected_true_only_on_exact_nonempty_match() {
    let (code, out, err) = run_sourced(
        r#"
        for a in "1.7.0-dev.244:1.7.0-dev.244:YES" "1.7.0-dev.244:1.7.0-dev.243:NO" ":1.7.0:NO" "1.7.0::NO"; do
          actual="${a%%:*}"; rest="${a#*:}"; expected="${rest%%:*}"; want="${rest#*:}"
          if version_matches_expected "$actual" "$expected"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $a" || echo "MISMATCH $a got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "version_matches_expected produced a mismatch: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) systemd service active
// ---------------------------------------------------------------------------------------------

#[test]
fn active_state_is_active_true_only_for_exact_active() {
    let (code, out, err) = run_sourced(
        r#"
        for s in "active" "inactive" "failed" "activating" ""; do
          if active_state_is_active "$s"; then echo "YES:$s"; else echo "NO:$s"; fi
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec![
            "YES:active",
            "NO:inactive",
            "NO:failed",
            "NO:activating",
            "NO:"
        ],
        "active_state_is_active must accept ONLY the exact 'active' state"
    );
}

// ---------------------------------------------------------------------------------------------
// (c) NDI emit + FATAL scan (reuses scripts/lib/ndi-alive.sh)
// ---------------------------------------------------------------------------------------------

const CAMERA_BOX_JOURNAL_HEALTHY: &str = "\
Jul 05 10:00:01 CAM5 camera-box[812]: Streaming: 60.0 fps emitted / 60.0 fps captured (300 sent, 300 captured, 0 capture-dropped)
Jul 05 10:00:01 CAM5 camera-box[812]: capture chroma: u_dev=14.2 v_dev=9.8 -> colour
Jul 05 10:00:06 CAM5 camera-box[812]: Streaming: 60.0 fps emitted / 60.0 fps captured (300 sent, 300 captured, 0 capture-dropped)
Jul 05 10:00:06 CAM5 camera-box[812]: capture chroma: u_dev=13.9 v_dev=10.1 -> colour
";

const CAMERA_BOX_JOURNAL_PANIC: &str = "\
Jul 05 10:00:01 CAM5 camera-box[812]: Streaming: 60.0 fps emitted / 60.0 fps captured (300 sent, 300 captured, 0 capture-dropped)
Jul 05 10:00:07 CAM5 camera-box[812]: thread 'main' panicked at 'called `Option::unwrap()` on a `None` value'
";

#[test]
fn ndi_emit_ok_true_on_genlock_report() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_emit_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        CAMERA_BOX_JOURNAL_HEALTHY.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn ndi_emit_ok_false_when_no_streaming_line() {
    let (code, out, err) = run_sourced(
        r#"TEXT='Jul 05 10:00:01 CAM5 camera-box[812]: starting up'
           if ndi_emit_ok "$TEXT"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

#[test]
fn ndi_journal_has_fatal_detects_panic_and_ignores_healthy_log() {
    let (code, out, err) = run_sourced(&format!(
        "HEALTHY='{}'\nPANIC='{}'\n\
         if ndi_journal_has_fatal \"$HEALTHY\"; then echo HEALTHY_FATAL; else echo HEALTHY_OK; fi\n\
         if ndi_journal_has_fatal \"$PANIC\"; then echo PANIC_FATAL; else echo PANIC_OK; fi",
        CAMERA_BOX_JOURNAL_HEALTHY.replace('\'', "'\\''"),
        CAMERA_BOX_JOURNAL_PANIC.replace('\'', "'\\''"),
    ));
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["HEALTHY_OK", "PANIC_FATAL"]);
}

// ---------------------------------------------------------------------------------------------
// (i) colour capture chroma metric (#299)
// ---------------------------------------------------------------------------------------------

#[test]
fn chroma_state_from_journal_picks_the_last_sample() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nchroma_state_from_journal \"$TEXT\"",
        CAMERA_BOX_JOURNAL_HEALTHY.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "capture chroma: u_dev=13.9 v_dev=10.1 -> colour"
    );
}

#[test]
fn chroma_check_distinguishes_colour_grayscale_and_unknown() {
    let (code, out, err) = run_sourced(
        r#"
        rc=0; chroma_check "capture chroma: u_dev=1.0 v_dev=1.0 -> colour" || rc=$?; echo "colour=$rc"
        rc=0; chroma_check "capture chroma: u_dev=0.1 v_dev=0.1 -> grayscale (source likely monochrome)" || rc=$?; echo "gray=$rc"
        rc=0; chroma_check "" || rc=$?; echo "unknown=$rc"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "colour=0\ngray=2\nunknown=3");
}

// ---------------------------------------------------------------------------------------------
// (d) dantesync locked + offset (reuses scripts/clock-offset-guard.sh)
// ---------------------------------------------------------------------------------------------

const DANTESYNC_LOCKED_JOURNAL: &str = "\
Jul 05 10:00:01 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
Jul 05 10:00:02 CAM5 dantesync[900]: [NTP] offset:+300us (threshold:520us, adaptive)
Jul 05 10:00:03 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

const DANTESYNC_DEGRADED_JOURNAL: &str = "\
Jul 05 10:00:01 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
Jul 05 10:00:02 CAM5 dantesync[900]: [NTP] offset:+300us (threshold:520us, adaptive)
";

#[test]
fn dantesync_locked_ok_true_when_servo_is_the_most_recent_event() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif dantesync_locked_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        DANTESYNC_LOCKED_JOURNAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn dantesync_locked_ok_false_when_ntp_line_is_the_most_recent_event() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif dantesync_locked_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        DANTESYNC_DEGRADED_JOURNAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

// ---------------------------------------------------------------------------------------------
// (d) FRESH offset verdict (#550/#591) — supersedes the pre-#591 dantesync_offset_ok, which read
// the LAST "[NTP] offset:" line via tail -1 regardless of AGE. On cam5/6 that graded on a STALE
// boot-STEP line ("[NTP] offset:-5280959us"), not the current offset (#550). dantesync_offset_verdict
// reads the FRESHEST offset line, REJECTS it if older than a freshness bound behind the newest
// journal line, and only then checks |offset| against the bound. Fixtures use the real
// `journalctl -o short-iso` timestamp form (colon in the TZ offset, e.g. +02:00).
// ---------------------------------------------------------------------------------------------

// Fresh, in-bound: the freshest [NTP] offset line is +14us, ~1s behind the newest PTP line.
const DS_FRESH_OK: &str = "\
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:45+02:00 CAM5 dantesync[900]: [NTP] offset:+14us (threshold:535us, adaptive)
2026-07-07T18:36:46+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

// The real cam5/6 desync: a FRESH [NTP] offset:-5280959us (a competing timesyncd stepping the
// clock -> a genuine 5.28s error), ~1s behind the newest PTP line.
const DS_FRESH_DRIFT: &str = "\
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:45+02:00 CAM5 dantesync[900]: [NTP] offset:-5280959us
2026-07-07T18:36:46+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

// A STALE boot-step offset line (#550): the ONLY [NTP] offset line is >1h behind the newest PTP
// line, so it must NOT be read as the current offset (the pre-#591 tail -1 bug graded on exactly
// this stale value).
const DS_STALE: &str = "\
2026-07-07T17:20:13+02:00 CAM6 dantesync[900]: [NTP] offset:-4357480us
2026-07-07T18:36:44+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:46+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

// No [NTP] offset line at all — only PTP servo lines.
const DS_ABSENT: &str = "\
2026-07-07T18:36:44+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:46+02:00 CAM6 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

fn offset_verdict(journal: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\ndantesync_offset_verdict \"$TEXT\" 300 2000",
        journal.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

#[test]
fn dantesync_offset_verdict_ok_on_fresh_in_bound_offset() {
    assert_eq!(offset_verdict(DS_FRESH_OK), "ok");
}

#[test]
fn dantesync_offset_verdict_drift_on_fresh_large_offset() {
    // The cam5/6 5.28s desync — a FRESH out-of-bound offset is a hard DRIFT (#591). A bare
    // "[NTP] offset:-5280959us" (no adaptive-threshold suffix) is the out-of-range fallback form.
    assert_eq!(offset_verdict(DS_FRESH_DRIFT), "drift");
}

#[test]
fn dantesync_offset_verdict_rejects_stale_boot_step_line() {
    // #550: the freshest [NTP] offset line is a >1h-old boot STEP; it must NOT be read as the
    // current offset (the pre-#591 tail -1 bug graded on exactly this stale value). Verdict is
    // "stale", never "drift" (a stale value must never look like a real current desync) nor "ok".
    assert_eq!(offset_verdict(DS_STALE), "stale");
}

#[test]
fn dantesync_offset_verdict_absent_when_no_offset_line() {
    assert_eq!(offset_verdict(DS_ABSENT), "absent");
}

// ---------------------------------------------------------------------------------------------
// (d) dantesync LIVENESS gate (#600 / #591 review) — the offset/lock verdicts above grade the
// journal's CONTENT, but a died/hung dantesync leaves BOTH signals computed against a STALE
// journal and PASSES (the clock has been free-running/undisciplined the whole time). Two pure
// helpers, gated AHEAD of the lock/offset reads:
//   dantesync_service_active STATE   -> 0 iff STATE is exactly "active"          (catches DIED)
//   dantesync_journal_fresh J NOW MAX -> "fresh"|"stale" — the newest journal line must be within
//                                        MAX seconds of the box's OWN wall clock (catches HUNG-
//                                        but-still-"active", journal not advancing).
// ---------------------------------------------------------------------------------------------

fn service_active(state: &str) -> bool {
    let (code, out, err) = run_sourced(&format!(
        "if dantesync_service_active '{}'; then echo YES; else echo NO; fi",
        state.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim() == "YES"
}

#[test]
fn dantesync_service_active_true_only_on_exactly_active() {
    assert!(
        service_active("active"),
        "a running dantesync ('active') is the ONLY live state"
    );
    // Every non-"active" systemctl is-active state means the clock is undisciplined/free-running.
    for s in [
        "inactive",
        "failed",
        "activating",
        "deactivating",
        "",
        "unknown",
    ] {
        assert!(
            !service_active(s),
            "'{s}' must NOT count as a live dantesync (clock free-running -> hard FAIL)"
        );
    }
}

// A dantesync journal whose NEWEST line is at 18:36:45+02:00 (epoch 1783442205). box_now is derived
// from that same instant via `date -d` in the harness, so the (box_now - newest) age is exact and
// timezone-independent — no brittle hand-computed epoch. Both timestamps are the BOX's own; the
// verifier host's clock never enters the comparison.
const DS_ADVANCING_JOURNAL: &str = "\
2026-07-07T18:36:44+02:00 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
2026-07-07T18:36:45+02:00 CAM5 dantesync[900]: [NTP] offset:+14us (threshold:535us, adaptive)
";
// No ISO-timestamped line at all — the newest-line epoch is unextractable -> fail-closed to stale.
const DS_NO_ISO_LINE: &str = "\
some dantesync line with no leading short-iso timestamp
another such line
";

// journal_fresh_rel: box_now = (epoch of the journal's newest line, 18:36:45+02:00) + plus_secs,
// computed by the SAME `date -d` the helper uses, so the age is exactly `plus_secs`.
fn journal_fresh_rel(journal: &str, plus_secs: i64, max_age: i64) -> String {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nNOW=$(( $(date -d '2026-07-07 18:36:45 +02:00' +%s) + ({plus_secs}) ))\n\
         dantesync_journal_fresh \"$TEXT\" \"$NOW\" {max_age}",
        journal.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

// journal_fresh_abs: explicit (possibly garbage) box_now string, to prove the fail-closed paths.
fn journal_fresh_abs(journal: &str, box_now: &str, max_age: i64) -> String {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\ndantesync_journal_fresh \"$TEXT\" '{}' {max_age}",
        journal.replace('\'', "'\\''"),
        box_now.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

#[test]
fn dantesync_journal_fresh_when_newest_line_is_within_max_age() {
    // Newest line 5s before the box clock (max 60) -> the journal is advancing -> fresh.
    assert_eq!(journal_fresh_rel(DS_ADVANCING_JOURNAL, 5, 60), "fresh");
}

#[test]
fn dantesync_journal_fresh_stale_when_journal_stopped_advancing() {
    // Newest line 600s before the box clock (max 60) -> daemon hung / not logging -> stale.
    assert_eq!(journal_fresh_rel(DS_ADVANCING_JOURNAL, 600, 60), "stale");
}

#[test]
fn dantesync_journal_fresh_not_stale_on_negative_age() {
    // Box clock 30s BEHIND the newest journal line (a stepped-backward wall clock). This helper
    // only catches "not advancing" — a stepped clock is (r)/drift's job — so a negative age is
    // NOT stale here.
    assert_eq!(journal_fresh_rel(DS_ADVANCING_JOURNAL, -30, 60), "fresh");
}

#[test]
fn dantesync_journal_fresh_fail_closed_on_bad_box_now() {
    // Empty / non-numeric box_now cannot prove freshness -> fail-closed to stale.
    assert_eq!(journal_fresh_abs(DS_ADVANCING_JOURNAL, "", 60), "stale");
    assert_eq!(
        journal_fresh_abs(DS_ADVANCING_JOURNAL, "notanumber", 60),
        "stale"
    );
}

#[test]
fn dantesync_journal_fresh_stale_when_no_parseable_line() {
    // No extractable newest-line timestamp -> cannot prove the journal advanced -> stale.
    assert_eq!(journal_fresh_abs(DS_NO_ISO_LINE, "9999999999", 60), "stale");
}

// ---------------------------------------------------------------------------------------------
// (r) single timesync authority: dantesync ONLY, no competing daemon installed/active/enabled (#591)
// ---------------------------------------------------------------------------------------------

fn authority_verdict(block: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "BLOCK='{}'\ntimesync_authority_verdict \"$BLOCK\"",
        block.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

#[test]
fn timesync_authority_verdict_ok_on_clean_box() {
    // Every competing daemon not-installed (dpkg empty), inactive, no enabled state -> dantesync
    // is the sole clock authority. Block format: NAME|DPKG_STATUS|IS_ACTIVE|IS_ENABLED.
    let block = "\
systemd-timesyncd||inactive|
chrony||inactive|
ntp||inactive|
ntpsec||inactive|
openntpd||inactive|";
    assert_eq!(authority_verdict(block), "ok");
}

#[test]
fn timesync_authority_verdict_ok_on_the_real_post_provisioning_steady_state() {
    // #591 review: the fixture above only exercises "purged" (empty dpkg, empty enabled) OR
    // "installed" (which short-circuits on dpkg alone before the enabled-state logic is ever
    // reached) -- neither pins the ACTUAL steady state setup-device.sh / create-usb-linux.sh
    // produce after a successful purge, since both ALSO run `systemctl mask` as an unconditional
    // backstop even when the purge succeeds: dpkg="" (purged) but enabled=masked (not empty).
    // Confirms timesync_enabled_state_neutral's "masked" branch is actually reached (and passes)
    // on a genuinely-purged daemon, not just short-circuited past by the dpkg check.
    let block = "\
systemd-timesyncd||inactive|masked
chrony||inactive|masked
ntp||inactive|masked
ntpsec||inactive|masked
openntpd||inactive|masked";
    assert_eq!(authority_verdict(block), "ok");
}

#[test]
fn timesync_authority_verdict_fails_on_cam5_cam6_timesyncd() {
    // The real cam5/6 failure: systemd-timesyncd installed + active + enabled ALONGSIDE dantesync.
    let block = "\
systemd-timesyncd|install ok installed|active|enabled
chrony||inactive|
ntp||inactive|
ntpsec||inactive|
openntpd||inactive|";
    let v = authority_verdict(block);
    assert_ne!(v, "ok", "cam5/6 double-daemon must FAIL");
    assert!(
        v.contains("FAIL:") && v.contains("systemd-timesyncd"),
        "verdict must name the offender: {v}"
    );
}

#[test]
fn timesync_authority_verdict_fails_on_masked_but_installed() {
    // Masking is NOT enough — a minimalist appliance PURGES it. installed = FAIL even when masked
    // and inactive (the cam1-4 "installed-but-disabled/masked" state this gate now rejects).
    let block = "systemd-timesyncd|install ok installed|inactive|masked";
    let v = authority_verdict(block);
    assert_ne!(
        v, "ok",
        "masked-but-installed must still FAIL (purge, don't mask)"
    );
    assert!(
        v.contains("systemd-timesyncd"),
        "must name the offender: {v}"
    );
}

#[test]
fn timesync_daemon_verdict_ok_only_when_absent_inactive_neutral() {
    let (code, out, err) = run_sourced(
        r#"
        # absent (empty dpkg), inactive, empty enabled -> ok
        timesync_daemon_verdict systemd-timesyncd "" inactive ""
        # installed (even masked + inactive) -> FAIL (purge, not mask)
        timesync_daemon_verdict systemd-timesyncd "install ok installed" inactive masked
        # not installed but ACTIVE -> FAIL
        timesync_daemon_verdict chrony "" active ""
        # not installed but ENABLED (not neutral) -> FAIL
        timesync_daemon_verdict ntp "" inactive enabled
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "ok", "absent/inactive/neutral must be ok");
    assert!(
        lines[1].contains("INSTALLED"),
        "installed must FAIL: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("ACTIVE"),
        "active must FAIL: {}",
        lines[2]
    );
    assert!(
        lines[3].contains("enabled"),
        "enabled must FAIL: {}",
        lines[3]
    );
}

// dpkg_status_installed edge cases (independent-review finding, #591): the ORIGINAL implementation
// matched ONLY the exact "install ok installed" triad, missing every other files-present dpkg
// state (a held package, or one caught mid-unpack/configure) -- all of which leave the competing
// daemon's binary+unit on disk, exactly what this check exists to reject. Verified via the pure
// function directly (not just the composed timesync_daemon_verdict) so the dpkg-state contract is
// pinned on its own.
#[test]
fn dpkg_status_installed_true_for_every_files_present_state_false_only_when_genuinely_gone() {
    let (code, out, err) = run_sourced(
        r#"
        for st in \
          "install ok installed" \
          "hold ok installed" \
          "install ok unpacked" \
          "install ok half-configured" \
          "install ok half-installed" \
          "install ok triggers-pending" \
          "install ok triggers-awaiting" \
          "deinstall ok config-files" \
          "purge ok not-installed" \
          ""; do
          if dpkg_status_installed "$st"; then echo "YES:$st"; else echo "NO:$st"; fi
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    // Files-present states -> YES (installed).
    for (i, st) in [
        "install ok installed",
        "hold ok installed",
        "install ok unpacked",
        "install ok half-configured",
        "install ok half-installed",
        "install ok triggers-pending",
        "install ok triggers-awaiting",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            lines[i],
            format!("YES:{st}"),
            "'{st}' leaves files on disk -- must be treated as installed"
        );
    }
    // Genuinely-gone states -> NO (not installed).
    for (i, st) in ["deinstall ok config-files", "purge ok not-installed", ""]
        .iter()
        .enumerate()
    {
        let idx = 7 + i;
        assert_eq!(
            lines[idx],
            format!("NO:{st}"),
            "'{st}' means the package is genuinely gone"
        );
    }
}

#[test]
fn timesync_daemon_verdict_fails_on_held_or_partially_configured_daemon() {
    // Regression for the independent-review finding: a held or mid-configure competing daemon
    // (files present, not the exact "install ok installed" string) must still FAIL through the
    // composed timesync_daemon_verdict, not just the isolated dpkg_status_installed.
    let (code, out, err) = run_sourced(
        r#"
        timesync_daemon_verdict systemd-timesyncd "hold ok installed" inactive masked
        timesync_daemon_verdict systemd-timesyncd "install ok unpacked" inactive ""
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].contains("INSTALLED"),
        "held package must FAIL: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("INSTALLED"),
        "mid-unpack package must FAIL: {}",
        lines[1]
    );
}

// ---------------------------------------------------------------------------------------------
// (e) genlock.conf drop-in FPS
// ---------------------------------------------------------------------------------------------

#[test]
fn genlock_dropin_fps_parses_the_value() {
    let (code, out, err) = run_sourced(
        r#"TEXT='[Service]
Environment=CAMERA_BOX_GENLOCK_FPS=60'
           genlock_dropin_fps "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "60");
}

#[test]
fn genlock_dropin_fps_empty_when_missing() {
    let (code, out, err) = run_sourced(r#"genlock_dropin_fps """#);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "");
}

#[test]
fn genlock_fps_matches_true_only_on_exact_match() {
    let (code, out, err) = run_sourced(
        r#"
        if genlock_fps_matches "60" "60"; then echo YES; else echo NO; fi
        if genlock_fps_matches "30" "60"; then echo YES; else echo NO; fi
        if genlock_fps_matches "" "60"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES\nNO\nNO");
}

// ---------------------------------------------------------------------------------------------
// (f) cpu-affinity.conf drop-in
// ---------------------------------------------------------------------------------------------

#[test]
fn cpu_affinity_dropin_value_parses_the_value() {
    let (code, out, err) = run_sourced(
        r#"TEXT='[Service]
# #289: pin grab to the isolated core (isolcpus=3) so box load never starves capture/emit
CPUAffinity=3'
           cpu_affinity_dropin_value "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "3");
}

// ---------------------------------------------------------------------------------------------
// (z) publish-30p.conf drop-in + live "CAMn (30p)" blend stream (issue 792 feature, baked into
// provisioning by #1087). Two facets: the drop-in enables the secondary 30fps stream, AND the
// box must actually be publishing it (an old binary predating issue 792 still FAILs facet 2).
// ---------------------------------------------------------------------------------------------

#[test]
fn publish_30p_dropin_value_parses_the_value() {
    let (code, out, err) = run_sourced(
        r#"TEXT='[Service]
Environment=CAMERA_BOX_PUBLISH_30P=1'
           publish_30p_dropin_value "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "1");
}

#[test]
fn publish_30p_dropin_value_empty_when_missing() {
    let (code, out, err) = run_sourced(r#"publish_30p_dropin_value """#);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "");
}

#[test]
fn publish_30p_stream_live_counts_publisher_activity() {
    // Both the one-shot startup line and the recurring per-interval output line count as "the
    // (30p) stream is genuinely being published" -- mirrors the live cam1/cam2 journal shape.
    let (code, out, err) = run_sourced(
        r#"TEXT='INFO camera_box: #792 publish-30p ACTIVE: streaming as 30p (blend=0.5, channel depth 4)
INFO camera_box::publish_30p: #792 publish-30p: 300 outputs (300 blended, 0 solo), 0 tee drops'
           publish_30p_stream_live "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "2");
}

#[test]
fn publish_30p_stream_live_zero_when_stream_absent() {
    // A box publishing only the primary 'usb' source (no publish-30p publisher lines) reports 0
    // -> the (z) check FAILs facet 2 even when the drop-in is on disk (e.g. an old binary).
    let (code, out, err) = run_sourced(
        r#"TEXT='INFO camera_box: NDI sender ready, streaming as usb'
           publish_30p_stream_live "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "0");
}

#[test]
fn check_publish_30p_is_wired_into_the_live_flow() {
    // The pure parsers above are dead unless the live flow CALLs them -- assert the real call
    // sites (with their args) appear AFTER the source-guard, and that the check list advertises
    // the new (z) letter. Same non-tautological pattern as check_q_is_wired / dantesync-liveness.
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment must still be present");
    let live_flow = &body[guard_pos..];
    assert!(
        live_flow.contains("publish_30p_dropin_value \"$P30_CONF\""),
        "the live flow must CALL publish_30p_dropin_value on the publish-30p.conf drop-in (#1087)"
    );
    assert!(
        live_flow.contains("publish_30p_stream_live \"$CB_JOURNAL\""),
        "the live flow must CALL publish_30p_stream_live over CB_JOURNAL to prove the (30p) stream \
         is genuinely being published, not merely enabled on disk (#1087)"
    );
    assert!(
        live_flow.contains("(z)"),
        "the check list / (z) block must advertise the new publish-30p acceptance check (#1087)"
    );
}

// ---------------------------------------------------------------------------------------------
// (g) libndi root-owned symlink chain
// ---------------------------------------------------------------------------------------------

const NDI_LS_CANONICAL: &str = "\
total 556
drwxr-xr-x 2 root root   4096 Jul  5 10:00 .
drwxr-xr-x 3 root root   4096 Jul  5 10:00 ..
lrwxrwxrwx 1 root root     12 Jul  5 10:00 libndi.so -> libndi.so.6
lrwxrwxrwx 1 root root     20 Jul  5 10:00 libndi.so.6 -> libndi.so.6.3.2.0
-rwxr-xr-x 1 root root 545280 Jul  5 10:00 libndi.so.6.3.2.0
";

// The #445 cam3-outlier layout: real files, user-owned (its manual NDI upgrade never fit the
// fleet script) -- verify-device.sh certifies the CANONICAL build, so this must FAIL.
const NDI_LS_CAM3_OUTLIER: &str = "\
total 556
drwxr-xr-x 2 newlevel newlevel   4096 Jul  5 10:00 .
drwxr-xr-x 3 newlevel newlevel   4096 Jul  5 10:00 ..
-rwxr-xr-x 1 newlevel newlevel     12 Jul  5 10:00 libndi.so
-rwxr-xr-x 1 newlevel newlevel 545280 Jul  5 10:00 libndi.so.6
";

const NDI_LS_NON_ROOT_SYMLINK: &str = "\
total 556
drwxr-xr-x 2 root root   4096 Jul  5 10:00 .
drwxr-xr-x 3 root root   4096 Jul  5 10:00 ..
lrwxrwxrwx 1 newlevel newlevel 20 Jul  5 10:00 libndi.so.6 -> libndi.so.6.3.2.0
-rwxr-xr-x 1 root root 545280 Jul  5 10:00 libndi.so.6.3.2.0
";

#[test]
fn ndi_symlink_chain_ok_true_on_canonical_layout() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_symlink_chain_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        NDI_LS_CANONICAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn ndi_symlink_chain_ok_false_on_cam3_outlier_real_file_layout() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_symlink_chain_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        NDI_LS_CAM3_OUTLIER.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "NO",
        "the #445 cam3-outlier real-file layout must FAIL the canonical-build gate"
    );
}

#[test]
fn ndi_symlink_chain_ok_false_when_symlink_is_not_root_owned() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_symlink_chain_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        NDI_LS_NON_ROOT_SYMLINK.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

// ---------------------------------------------------------------------------------------------
// (h) avahi mDNS NDI discovery
// ---------------------------------------------------------------------------------------------

const AVAHI_BROWSE_WITH_CAM5: &str = "\
+;eth0;IPv4;CAM1 (usb);_ndi._tcp;local
+;eth0;IPv4;CAM5 (usb);_ndi._tcp;local
";

#[test]
fn avahi_ndi_discoverable_true_when_source_present() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif avahi_ndi_discoverable \"$TEXT\" \"CAM5\"; then echo YES; else echo NO; fi",
        AVAHI_BROWSE_WITH_CAM5.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn avahi_ndi_discoverable_false_when_source_absent() {
    // Any source not present in the browse text must read as absent -- "CAM4" is a real fleet
    // camera, simply not present in this particular browse snippet (which only lists CAM1/CAM5);
    // it is deliberately NOT "CAM7" (#593: cam7 was never built and is not part of the fleet).
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif avahi_ndi_discoverable \"$TEXT\" \"CAM4\"; then echo YES; else echo NO; fi",
        AVAHI_BROWSE_WITH_CAM5.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

#[test]
fn avahi_ndi_discoverable_false_on_empty_browse_output() {
    let (code, out, err) =
        run_sourced(r#"if avahi_ndi_discoverable "" "CAM5"; then echo YES; else echo NO; fi"#);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

// ---------------------------------------------------------------------------------------------
// (j)-(o) fleet-uniformity invariants (#547) — every cambox identical: ro-root, ONE kernel, no
// fwupd, wait-online masked, #289/#303 core-isolation cmdline, pinned NDI runtime.
// ---------------------------------------------------------------------------------------------

#[test]
fn root_mount_is_readonly_true_only_when_first_option_is_ro() {
    // A rw mount that carries "errors=remount-ro" in its options must NOT read as read-only —
    // the kernel always emits ro/rw as the FIRST comma-token, so only that decides.
    let (code, out, err) = run_sourced(
        r#"
        for o in "ro,relatime:YES" "rw,relatime:NO" "rw,errors=remount-ro:NO" "ro:YES" ":NO"; do
          opts="${o%%:*}"; want="${o##*:}"
          if root_mount_is_readonly "$opts"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $o" || echo "MISMATCH $o got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "root_mount_is_readonly mismatch: {out}"
    );
}

#[test]
fn kernels_uniform_ok_true_only_for_single_installed_matching_running() {
    let (code, out, err) = run_sourced(
        r#"
        ONE='/boot/vmlinuz-6.8.0-134-generic'
        TWO='/boot/vmlinuz-6.8.0-134-generic
/boot/vmlinuz-6.8.0-90-generic'
        if kernels_uniform_ok "$ONE" "6.8.0-134-generic"; then echo YES; else echo NO; fi
        if kernels_uniform_ok "$TWO" "6.8.0-134-generic"; then echo YES; else echo NO; fi
        if kernels_uniform_ok "$ONE" "6.8.0-90-generic"; then echo YES; else echo NO; fi
        if kernels_uniform_ok "" "6.8.0-134-generic"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "YES\nNO\nNO\nNO",
        "single-installed-kernel==running only; two kernels (cam4 drift) or a mismatch must FAIL"
    );
}

#[test]
fn fwupd_absent_true_only_when_purged() {
    // The fleet PURGES fwupd (it held a write handle blocking the ro remount). A unit still
    // present in ANY state — including masked — is not identical to a purged box, so FAILs.
    let (code, out, err) = run_sourced(
        r#"
        for s in "not-found:YES" ":YES" "static:NO" "enabled:NO" "masked:NO" "disabled:NO"; do
          st="${s%%:*}"; want="${s##*:}"
          if fwupd_absent "$st"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $s" || echo "MISMATCH $s got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(!out.contains("MISMATCH"), "fwupd_absent mismatch: {out}");
}

#[test]
fn waitonline_masked_true_only_when_masked() {
    let (code, out, err) = run_sourced(
        r#"
        for s in "masked:YES" "enabled:NO" "disabled:NO" ":NO" "not-found:NO"; do
          st="${s%%:*}"; want="${s##*:}"
          if waitonline_masked "$st"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $s" || echo "MISMATCH $s got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "waitonline_masked mismatch: {out}"
    );
}

const CMDLINE_FULL: &str = "BOOT_IMAGE=/boot/vmlinuz-6.8.0-134-generic root=UUID=abc ro quiet isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2";
const CMDLINE_PARTIAL: &str =
    "BOOT_IMAGE=/boot/vmlinuz-6.8.0-134-generic root=UUID=abc ro quiet isolcpus=3 nohz_full=3";

#[test]
fn cmdline_has_isolation_requires_all_four_flags() {
    let (code, out, err) = run_sourced(&format!(
        "FULL='{}'\nPARTIAL='{}'\n\
         if cmdline_has_isolation \"$FULL\"; then echo FULL_YES; else echo FULL_NO; fi\n\
         if cmdline_has_isolation \"$PARTIAL\"; then echo PARTIAL_YES; else echo PARTIAL_NO; fi\n\
         if cmdline_has_isolation \"\"; then echo EMPTY_YES; else echo EMPTY_NO; fi",
        CMDLINE_FULL, CMDLINE_PARTIAL,
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "FULL_YES\nPARTIAL_NO\nEMPTY_NO");
}

#[test]
fn cmdline_has_isolation_matches_whole_tokens_not_prefixes() {
    // nohz_full=3 must NOT be satisfied by nohz_full=30 (whole-token match).
    let (code, out, err) = run_sourced(
        r#"BOGUS='ro isolcpus=3 nohz_full=30 rcu_nocbs=3 irqaffinity=0-2'
           if cmdline_has_isolation "$BOGUS"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

// --- issue 899: realtime-isolation check (ac) pure helpers -----------------------------------

#[test]
fn cpulist_contains_matches_ranges_lists_and_singles() {
    // /proc/irq/<n>/smp_affinity_list renders "3" / "0-2" / "0,2-3" — membership must handle all.
    let (code, out, err) = run_sourced(
        r#"chk() { if cpulist_contains "$1" "$2"; then echo IN; else echo OUT; fi; }
           chk "3" "3"
           chk "0-2" "3"
           chk "0-2" "1"
           chk "0,1,2" "3"
           chk "0,2-3" "3"
           chk "" "3"
           chk "0-3" "3"
"#,
    );
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert_eq!(out.trim(), "IN\nOUT\nIN\nOUT\nIN\nOUT\nIN");
}

#[test]
fn cpulist_max_picks_the_highest_core() {
    let (code, out, err) = run_sourced(
        r#"echo "$(cpulist_max "3")"
           echo "$(cpulist_max "2-3")"
           echo "$(cpulist_max "0,2-3")"
           echo "max=$(cpulist_max "")"
"#,
    );
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert_eq!(out.trim(), "3\n3\n3\nmax=");
}

#[test]
fn rt_irq_placement_verdict_grades_by_kernel_and_core() {
    // Defect 3 core logic: non-RT must route the capture IRQ OFF the grab core; RT co-locates it.
    let (code, out, err) = run_sourced(
        r#"rt_irq_placement_verdict 0 "3" "3"; echo
           rt_irq_placement_verdict 0 "0-2" "3"; echo
           rt_irq_placement_verdict 1 "3" "3"; echo
           rt_irq_placement_verdict 1 "0-2" "3"; echo
           rt_irq_placement_verdict 0 "" "3"; echo
           rt_irq_placement_verdict 0 "3" ""; echo
"#,
    );
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert_eq!(
        out.trim(),
        "drift-on-grab\nok-off-grab\nok-on-grab\nrt-off-grab\nno-irq\nno-irq"
    );
}

#[test]
fn ndi_symlink_version_extracts_from_canonical_target() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nndi_symlink_version \"$TEXT\"",
        NDI_LS_CANONICAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "6.3.2.0");
}

#[test]
fn ndi_version_matches_accepts_pin_prefix_only() {
    // Pin "6.3.2" accepts the 3-part soname "6.3.2" and the 4-part SDK string "6.3.2.0", but
    // rejects "6.2.1" and the deceptive "6.3.20".
    let (code, out, err) = run_sourced(
        r#"
        if ndi_version_matches "6.3.2.0" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "6.3.2" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "6.2.1" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "6.3.20" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "" "6.3.2"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES\nYES\nNO\nNO\nNO");
}

#[test]
fn fwupd_verdict_unreadable_on_ssh_failure_ok_on_purged_present_on_installed() {
    // Regression for the #549-review 🟡: (l) called fwupd_absent directly, and fwupd_absent treats
    // an EMPTY state as "purged". A transient ssh failure on the (l) call ALSO yields empty stdout
    // (rc!=0) -> that was a false green. fwupd_verdict gates on rc: rc!=0 -> "unreadable" (FAIL),
    // EVEN when the state string would otherwise look purged.
    let (code, out, err) = run_sourced(
        r#"
        echo "rc1_empty=$(fwupd_verdict 1 '')"
        echo "rc255_looks_purged=$(fwupd_verdict 255 'not-found')"
        echo "ok_empty=$(fwupd_verdict 0 '')"
        echo "ok_notfound=$(fwupd_verdict 0 'not-found')"
        echo "present_static=$(fwupd_verdict 0 'static')"
        echo "present_enabled=$(fwupd_verdict 0 'enabled')"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "rc1_empty=unreadable\n\
         rc255_looks_purged=unreadable\n\
         ok_empty=ok\n\
         ok_notfound=ok\n\
         present_static=present\n\
         present_enabled=present"
    );
}

// ---------------------------------------------------------------------------------------------
// (q) .bak cruft drift -- WARNING only, never a FAIL (#453)
//
// Live fleet fingerprint (2026-07-06, issue #453): cam1/cam2/cam4 carry inert `.bak` leftovers
// from a manual NDI upgrade (`/usr/lib/ndi/libndi.so.6*.bak`) and a stale drop-in edit (cam1's
// `camera-box.service.d/genlock.conf.bak-30`). Neither is loaded by anything -- ldconfig never
// resolves a `.bak` suffix, systemd only reads `*.conf` -- so this is drift to SURFACE, never a
// functional defect to FAIL the box's acceptance gate on (the "gate on real signals" philosophy).
// setup-device.sh's cleanup_bak_cruft (#453) makes a freshly (re-)provisioned box self-heal; this
// check makes the drift visible on boxes provisioned BEFORE that fix landed.
// ---------------------------------------------------------------------------------------------

#[test]
fn bak_cruft_names_finds_ls_la_and_ls_1_style_entries() {
    // `ls -la` dump (the NDI dir, reusing the SAME listing check (g)/(o) already gather) --
    // symlinks render "name -> target"; only the cruft REGULAR .bak file should match, never the
    // live symlink chain.
    const NDI_LS: &str = "\
total 556
drwxr-xr-x 2 root root   4096 Jul  5 10:00 .
drwxr-xr-x 3 root root   4096 Jul  5 10:00 ..
lrwxrwxrwx 1 root root     12 Jul  5 10:00 libndi.so -> libndi.so.6
lrwxrwxrwx 1 root root     20 Jul  5 10:00 libndi.so.6 -> libndi.so.6.3.2.0
-rwxr-xr-x 1 root root 545280 Jul  5 10:00 libndi.so.6.3.2.0
-rw-r--r-- 1 root root   4213 Jul  3 09:00 libndi.so.6.2.1.bak
";
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nbak_cruft_names \"$TEXT\"",
        NDI_LS.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "libndi.so.6.2.1.bak",
        "must find ONLY the inert .bak regular file, never the live symlink chain entries"
    );
}

#[test]
fn bak_cruft_names_finds_bak_dash_n_suffixed_dropins() {
    // `ls -1` dump (the systemd drop-in dir) -- cam1's real `genlock.conf.bak-30` leftover.
    const DROPIN_LS: &str = "cpu-affinity.conf\ngenlock.conf\ngenlock.conf.bak-30\n";
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nbak_cruft_names \"$TEXT\"",
        DROPIN_LS.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "genlock.conf.bak-30",
        "must find the .bak-30 leftover, never the real *.conf drop-ins"
    );
}

#[test]
fn bak_cruft_names_empty_on_a_clean_listing() {
    const CLEAN_LS: &str = "cpu-affinity.conf\ngenlock.conf\n";
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nbak_cruft_names \"$TEXT\"",
        CLEAN_LS.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out, "", "a clean listing must report no cruft");
}

#[test]
fn bak_cruft_report_combines_both_dirs_with_full_paths() {
    const NDI_LS: &str = "total 4\n-rw-r--r-- 1 root root 4213 Jul 3 09:00 libndi.so.6.bak\n";
    const DROPIN_LS: &str = "genlock.conf\ngenlock.conf.bak-30\n";
    let (code, out, err) = run_sourced(&format!(
        "NDI='{}'\nDROPIN='{}'\nbak_cruft_report \"$NDI\" \"$DROPIN\"",
        NDI_LS.replace('\'', "'\\''"),
        DROPIN_LS.replace('\'', "'\\''"),
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "/usr/lib/ndi/libndi.so.6.bak\n/etc/systemd/system/camera-box.service.d/genlock.conf.bak-30",
        "bak_cruft_report must prefix each finding with its real absolute path"
    );
}

#[test]
fn bak_cruft_report_empty_when_both_dirs_are_clean() {
    let (code, out, err) = run_sourced(r#"bak_cruft_report "cpu-affinity.conf" "genlock.conf""#);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out, "", "clean dirs must report no cruft anywhere");
}

#[test]
fn check_q_is_wired_into_the_live_flow_as_a_warning_never_a_fail() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_marker = "never run the live SSH flow below.";
    let guard_pos = body
        .find(guard_marker)
        .expect("source-guard comment must still be present");
    let live_flow = &body[guard_pos..];

    assert!(
        live_flow.contains("bak_cruft_report"),
        "the LIVE FLOW (after the source-guard) must CALL bak_cruft_report (#453) -- not just \
         define it"
    );
    assert!(
        live_flow.contains("(q)"),
        "the usage doc / check list must advertise the new (q) check (#453)"
    );

    // The whole point of #453's rescope is that stale .bak cruft is a WARNING, never a FAIL --
    // find the (q) check's OWN implementation block. The marker `# (q) .bak cruft drift` appears
    // exactly once in live_flow today (the usage() doc uses a different phrasing, "(q) WARNING
    // only: ..."); rfind is used defensively so that if a doc header ever repeated the marker it
    // would still resolve to the LATTER, real per-check block. Confirm it calls `warn`, never
    // `fail`, on a cruft hit. (q) is the LAST check before the ALL CLEAR/VERIFY FAILED summary,
    // so the block runs to end-of-file.
    let q_marker = "# (q) .bak cruft drift";
    let q_pos = live_flow
        .rfind(q_marker)
        .expect("(q) check implementation block must be present in the live flow");
    let q_block = &live_flow[q_pos..];

    assert!(
        q_block.contains("warn \""),
        "check (q) must report cruft via warn(), never fail() -- inert .bak cruft must not fail \
         the acceptance gate. block: {q_block:?}"
    );
    assert!(
        !q_block.contains("fail \""),
        "check (q) must NEVER call fail() -- a hard FAIL would break #453's explicit \
         'warning, not a functional defect' design. block: {q_block:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Wiring — the (d) dantesync LIVENESS gate (#600) must actually be composed into the (d) live
// flow AHEAD of the lock/offset reads (a dead pure function nobody calls would leave the
// died/hung-daemon hole open). Non-tautological: assert the CALL SITES with their real args
// appear in the LIVE-FLOW portion only (the pure definitions trivially contain their own names).
// ---------------------------------------------------------------------------------------------

#[test]
fn check_dantesync_liveness_is_wired_into_the_d_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_marker = "never run the live SSH flow below.";
    let guard_pos = body
        .find(guard_marker)
        .expect("source-guard comment must still be present");
    let live_flow = &body[guard_pos..];

    // The two liveness signals are gathered over SSH from the box itself.
    assert!(
        live_flow.contains("systemctl is-active dantesync"),
        "the LIVE FLOW must gather `systemctl is-active dantesync` over SSH (#600) to detect a \
         DIED daemon -- not just define the helper"
    );
    assert!(
        live_flow.contains("date +%s"),
        "the LIVE FLOW must gather the box's own wall clock (`date +%s`) over SSH (#600) so \
         journal freshness is judged against the box, never the verifier host"
    );
    // Both helpers must be CALLED with their real args, ahead of the lock/offset reads.
    assert!(
        live_flow.contains("dantesync_service_active \"$DS_ACTIVE\""),
        "the LIVE FLOW must CALL dantesync_service_active on the gathered is-active state (#600) \
         -- a died dantesync (free-running clock) must hard-FAIL before the stale lock/offset \
         reads are ever trusted"
    );
    assert!(
        live_flow.contains(
            "dantesync_journal_fresh \"$DS_JOURNAL\" \"$BOX_NOW\" \"$DANTESYNC_JOURNAL_MAX_AGE_S\""
        ),
        "the LIVE FLOW must CALL dantesync_journal_fresh on the dantesync journal + box clock + \
         DANTESYNC_JOURNAL_MAX_AGE_S (#600) -- a hung-but-'active' daemon (journal not advancing) \
         must hard-FAIL before the stale reads are trusted"
    );
    // The gate must be a hard FAIL, not a warning, for both liveness holes.
    assert!(
        live_flow.contains("clock undisciplined/free-running (#591 review)")
            && live_flow.contains("daemon hung, clock free-running (#591 review)"),
        "both liveness holes (died + hung) must fail() with a #591-review-tagged reason (#600)"
    );
    // A transient ssh failure on the box-clock read must fail() with its OWN distinct message, not
    // be misattributed to the hung-daemon branch (review of #600): the box-clock read captures the
    // ssh rc separately, and an unreadable clock hard-FAILs on its own reason.
    assert!(
        live_flow.contains("ds_now_rc")
            && live_flow.contains("could not read the box wall clock over SSH"),
        "an unreadable box clock (ssh rc captured) must hard-FAIL with a DISTINCT reason, never \
         misattributed to 'daemon hung' (#600 review)"
    );
}

// ---------------------------------------------------------------------------------------------
// (s) /var/log tmpfs bounded against runaway growth -- size cap + frequent rotation check (#679)
//
// Every cam box's /var/log is a fixed 50MB tmpfs; the stock logrotate config only rotates on a
// weekly calendar with no `size` cap, so a chatty logger (dantesync's per-second [PTP] Drift line
// was ~65% of the fleet's volume) filled it in ~4-5 days and crashed cam2's camera-box.service
// (2026-07-11). log_bound_verdict (scripts/lib/log-bound.sh) requires BOTH a `size` cap on
// /etc/logrotate.d/rsyslog AND a systemd timer drop-in that checks far more often than the stock
// daily cadence.
// ---------------------------------------------------------------------------------------------

fn log_bound_verdict_of(block: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "BLOCK='{}'\nlog_bound_verdict \"$BLOCK\"",
        block.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

#[test]
fn log_bound_verdict_ok_when_size_cap_and_frequent_dropin_are_both_present() {
    let block = "\
LOGROTATE_RSYSLOG_SIZE=5M
LOGROTATE_FREQUENT_DROPIN=[Timer]|OnCalendar=|OnCalendar=*:0/15|AccuracySec=1min|";
    assert_eq!(log_bound_verdict_of(block), "ok");
}

#[test]
fn log_bound_verdict_fails_when_the_size_cap_is_missing() {
    let block = "\
LOGROTATE_RSYSLOG_SIZE=
LOGROTATE_FREQUENT_DROPIN=[Timer]|OnCalendar=|OnCalendar=*:0/15|AccuracySec=1min|";
    let v = log_bound_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("size"),
        "missing size cap must FAIL with a size-related reason, got: {v}"
    );
}

#[test]
fn log_bound_verdict_fails_when_the_frequent_dropin_is_missing() {
    let block = "\
LOGROTATE_RSYSLOG_SIZE=5M
LOGROTATE_FREQUENT_DROPIN=";
    let v = log_bound_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("99-camera-box-frequent.conf"),
        "missing frequent-check drop-in must FAIL naming the drop-in path, got: {v}"
    );
}

#[test]
fn log_bound_verdict_fails_when_the_dropin_exists_but_stays_on_the_stock_daily_cadence() {
    // A drop-in file present but with the WRONG content (e.g. a no-op override, or a future edit
    // dropping the short interval) must still FAIL -- presence alone is not proof.
    let block = "\
LOGROTATE_RSYSLOG_SIZE=5M
LOGROTATE_FREQUENT_DROPIN=[Timer]|OnCalendar=daily|";
    let v = log_bound_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("short OnCalendar"),
        "a drop-in with no short OnCalendar interval must still FAIL, got: {v}"
    );
}

#[test]
fn log_bound_gather_remote_snippet_prints_both_expected_keys() {
    let (code, out, err) = run_sourced("log_bound_gather_remote_snippet");
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert!(
        out.contains("LOGROTATE_RSYSLOG_SIZE=") && out.contains("LOGROTATE_FREQUENT_DROPIN="),
        "log_bound_gather_remote_snippet must print BOTH KEY= lines log_bound_verdict parses, \
         got: {out}"
    );
}

#[test]
fn log_bound_logrotate_config_carries_the_size_cap_and_all_six_log_paths() {
    let (code, out, err) = run_sourced("log_bound_logrotate_config");
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    for needle in [
        "/var/log/syslog",
        "/var/log/auth.log",
        "/var/log/kern.log",
        "size 5M",
        "rotate 1",
        "/usr/lib/rsyslog/rsyslog-rotate",
    ] {
        assert!(
            out.contains(needle),
            "log_bound_logrotate_config output must contain '{needle}', got: {out}"
        );
    }
}

#[test]
fn log_bound_logrotate_config_output_itself_passes_the_verdict_it_gates() {
    // Round-trip: the SAME content this generator writes to the box must itself parse as "ok"
    // through the size-cap half of log_bound_verdict's grep — proves the generator and the
    // verdict's parsing regex agree on the literal `size <N><unit>` shape.
    let (code, out, err) = run_sourced(
        r#"
        cfg="$(log_bound_logrotate_config)"
        echo "$cfg" | grep -oE 'size[[:space:]]+[0-9]+[kKmMgG]' | head -1 | awk '{print $2}'
        "#,
    );
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert_eq!(
        out.trim(),
        "5M",
        "the verdict's own size-cap grep must extract '5M' from log_bound_logrotate_config's output"
    );
}

#[test]
fn log_bound_timer_dropin_sets_a_short_oncalendar_interval() {
    let (code, out, err) = run_sourced("log_bound_timer_dropin");
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert!(
        out.contains("OnCalendar=*:0/"),
        "log_bound_timer_dropin must set a short OnCalendar interval, got: {out}"
    );
    // Round-trip against the verdict's own parsing shape (the gather snippet flattens newlines to
    // '|' before log_bound_verdict inspects it).
    let flattened = out.replace('\n', "|");
    assert!(
        flattened.contains("OnCalendar=*:0/"),
        "flattened drop-in content must still contain the short interval, got: {flattened}"
    );
}

// ---------------------------------------------------------------------------------------------
// (u) rsyslog PURGED + journald RuntimeMaxUse capped (#762)
//
// A live cam1 incident (2026-07-15) showed a full 50MB /var/log tmpfs put rsyslogd into a
// write-error feedback loop (~400 lines/s, 42.8% CPU), starving the camera-box send path badly
// enough to measurably drift NDI delivery timing. rsyslog is redundant on a read-only appliance
// (journald already captures everything) -- log_diet_provision_verdict (scripts/lib/log-diet.sh)
// requires rsyslog to be genuinely PURGED (not merely masked -- an installed-but-masked daemon
// can still be re-enabled) AND a journald RuntimeMaxUse=20M drop-in to be present.
// ---------------------------------------------------------------------------------------------

fn log_diet_verdict_of(block: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "BLOCK='{}'\nlog_diet_provision_verdict \"$BLOCK\"",
        block.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

#[test]
fn log_diet_verdict_ok_when_rsyslog_purged_and_journald_capped() {
    let block = "\
RSYSLOG_DPKG=
RSYSLOG_ACTIVE=inactive
RSYSLOG_ENABLED=
JOURNALD_DROPIN=[Journal]|RuntimeMaxUse=20M|";
    assert_eq!(log_diet_verdict_of(block), "ok");
}

#[test]
fn log_diet_verdict_ok_when_rsyslog_dpkg_reports_not_installed_or_config_files() {
    // Same "files genuinely gone" states timesync_daemon_verdict already accepts for a purged
    // competing daemon -- config-files (removed, only leftover conffiles) is still "purged".
    for dpkg_state in ["", "purge ok not-installed", "purge ok config-files"] {
        let block = format!(
            "\
RSYSLOG_DPKG={dpkg_state}
RSYSLOG_ACTIVE=inactive
RSYSLOG_ENABLED=
JOURNALD_DROPIN=[Journal]|RuntimeMaxUse=20M|"
        );
        assert_eq!(
            log_diet_verdict_of(&block),
            "ok",
            "dpkg state '{dpkg_state}' should read as purged"
        );
    }
}

#[test]
fn log_diet_verdict_fails_when_rsyslog_is_still_installed() {
    let block = "\
RSYSLOG_DPKG=install ok installed
RSYSLOG_ACTIVE=inactive
RSYSLOG_ENABLED=disabled
JOURNALD_DROPIN=[Journal]|RuntimeMaxUse=20M|";
    let v = log_diet_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("INSTALLED"),
        "an installed (even inactive/disabled) rsyslog must FAIL -- purge, not just mask, got: {v}"
    );
}

#[test]
fn log_diet_verdict_fails_when_rsyslog_is_masked_but_installed() {
    // Masking alone is explicitly NOT enough (mirrors timesync_daemon_verdict's own "masking is
    // not enough" contract) -- a masked-but-installed daemon can still be re-enabled.
    let block = "\
RSYSLOG_DPKG=install ok installed
RSYSLOG_ACTIVE=inactive
RSYSLOG_ENABLED=masked
JOURNALD_DROPIN=[Journal]|RuntimeMaxUse=20M|";
    let v = log_diet_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("INSTALLED"),
        "masked-but-installed rsyslog must still FAIL, got: {v}"
    );
}

#[test]
fn log_diet_verdict_fails_when_rsyslog_is_active() {
    let block = "\
RSYSLOG_DPKG=
RSYSLOG_ACTIVE=active
RSYSLOG_ENABLED=
JOURNALD_DROPIN=[Journal]|RuntimeMaxUse=20M|";
    let v = log_diet_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("ACTIVE"),
        "an active rsyslog must FAIL even if dpkg somehow reads empty, got: {v}"
    );
}

#[test]
fn log_diet_verdict_fails_when_journald_dropin_is_missing() {
    let block = "\
RSYSLOG_DPKG=
RSYSLOG_ACTIVE=inactive
RSYSLOG_ENABLED=
JOURNALD_DROPIN=";
    let v = log_diet_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("99-camera-box-diet.conf"),
        "a missing journald drop-in must FAIL naming the drop-in path, got: {v}"
    );
}

#[test]
fn log_diet_verdict_fails_when_journald_dropin_has_the_wrong_value() {
    // Present but WRONG content (e.g. a hand-edit widening the cap) must still FAIL --
    // presence alone is not proof, mirroring log_bound_verdict's own "wrong content" case.
    let block = "\
RSYSLOG_DPKG=
RSYSLOG_ACTIVE=inactive
RSYSLOG_ENABLED=
JOURNALD_DROPIN=[Journal]|RuntimeMaxUse=500M|";
    let v = log_diet_verdict_of(block);
    assert!(
        v.starts_with("FAIL:") && v.contains("RuntimeMaxUse=20M"),
        "a wrong-value journald drop-in must FAIL naming the expected value, got: {v}"
    );
}

#[test]
fn log_diet_verdict_reports_every_failure_not_just_the_first() {
    // A box that is BOTH still-installed-and-active AND missing the journald cap must surface
    // BOTH problems in one verdict -- the operator should not have to re-run verify-device.sh
    // twice to discover the second issue (mirrors timesync_authority_verdict's multi-fail shape).
    let block = "\
RSYSLOG_DPKG=install ok installed
RSYSLOG_ACTIVE=active
RSYSLOG_ENABLED=enabled
JOURNALD_DROPIN=";
    let v = log_diet_verdict_of(block);
    assert!(v.contains("INSTALLED"), "got: {v}");
    assert!(v.contains("ACTIVE"), "got: {v}");
    assert!(v.contains("enabled"), "got: {v}");
    assert!(v.contains("99-camera-box-diet.conf"), "got: {v}");
}

#[test]
fn log_diet_gather_remote_snippet_prints_all_four_expected_keys() {
    let (code, out, err) = run_sourced("log_diet_gather_remote_snippet");
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    for key in [
        "RSYSLOG_DPKG=",
        "RSYSLOG_ACTIVE=",
        "RSYSLOG_ENABLED=",
        "JOURNALD_DROPIN=",
    ] {
        assert!(
            out.contains(key),
            "log_diet_gather_remote_snippet must print '{key}', got: {out}"
        );
    }
}

#[test]
fn log_diet_journald_dropin_sets_the_pinned_runtime_max_use() {
    let (code, out, err) = run_sourced("log_diet_journald_dropin");
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert!(
        out.contains("[Journal]") && out.contains("RuntimeMaxUse=20M"),
        "log_diet_journald_dropin must set [Journal] RuntimeMaxUse=20M, got: {out}"
    );
    // Round-trip against the verdict's own parsing shape (the gather snippet flattens newlines to
    // '|' before log_diet_provision_verdict inspects it) -- proves the generator and the verdict
    // agree on the literal content shape, mirroring log_bound_timer_dropin's own round-trip test.
    let flattened = out.replace('\n', "|");
    assert!(
        flattened.contains("RuntimeMaxUse=20M"),
        "flattened drop-in content must still contain the cap value, got: {flattened}"
    );
}

// ---------------------------------------------------------------------------------------------
// #762 fix -- log_diet_rsyslog_purged: lets (s) supersede its #679 logrotate check once rsyslog
// is genuinely purged (its own conffile /etc/logrotate.d/rsyslog is removed WITH the package, so
// the OLD #679 check becomes structurally unsatisfiable on an otherwise-correctly-hardened box).
// ---------------------------------------------------------------------------------------------

fn rsyslog_purged(block: &str) -> bool {
    let (code, out, err) = run_sourced(&format!(
        "BLOCK='{}'\nif log_diet_rsyslog_purged \"$BLOCK\"; then echo YES; else echo NO; fi",
        block.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    match out.trim() {
        "YES" => true,
        "NO" => false,
        other => panic!("unexpected harness output: {other}"),
    }
}

#[test]
fn log_diet_rsyslog_purged_true_when_dpkg_reports_purged_states() {
    for dpkg_state in ["", "purge ok not-installed", "purge ok config-files"] {
        let block = format!("RSYSLOG_DPKG={dpkg_state}\nRSYSLOG_ACTIVE=inactive\nRSYSLOG_ENABLED=");
        assert!(
            rsyslog_purged(&block),
            "dpkg state '{dpkg_state}' should read as purged"
        );
    }
}

#[test]
fn log_diet_rsyslog_purged_false_when_still_installed_even_if_inactive_and_masked() {
    // Masking alone does NOT count as purged -- mirrors the (u) check's own "masking is not
    // enough" contract.
    let block =
        "RSYSLOG_DPKG=install ok installed\nRSYSLOG_ACTIVE=inactive\nRSYSLOG_ENABLED=masked";
    assert!(!rsyslog_purged(block));
}

#[test]
fn log_diet_rsyslog_purged_from_dpkg_matches_the_block_level_wrapper() {
    let (code, out, err) = run_sourced(
        "log_diet_rsyslog_purged_from_dpkg 'install ok installed' && echo YES || echo NO",
    );
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert_eq!(out.trim(), "NO");

    let (code, out, err) =
        run_sourced("log_diet_rsyslog_purged_from_dpkg '' && echo YES || echo NO");
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

// ---------------------------------------------------------------------------------------------
// #782 -- the (aa) interkom-audio acceptance check: by-NAME /etc/asound.conf + per-box Mic/PCM
// mixer gains + alsa-utils installed. Inserted BEFORE (q) (the intentionally-LAST check), sourcing
// the shared scripts/lib/interkom-audio.sh so the writer (setup-device.sh) and the verifier can
// never drift. Non-tautological: the pure functions are DEFINED above the source-guard, so any
// occurrence in the live-flow slice is a genuine CALL site.
// ---------------------------------------------------------------------------------------------

/// verify-device.sh must SOURCE the interkom-audio lib -- the same single source of truth
/// setup-device.sh writes from, so the acceptance gate checks exactly what provisioning bakes.
#[test]
fn verify_device_sources_interkom_audio_lib() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r#". "$HERE/lib/interkom-audio.sh""#),
        "verify-device.sh must source scripts/lib/interkom-audio.sh"
    );
}

/// The (aa) check must be WIRED into the live SSH flow: it reads /etc/asound.conf, the alsa-utils
/// dpkg status, and the live `amixer -c HID sget Mic`/`PCM`, and composes them through the lib's
/// parsers + per-box table -- not merely define anything.
#[test]
fn check_aa_is_wired_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment must still be present");
    let live_flow = &body[guard_pos..];
    for needle in [
        "cat /etc/asound.conf",
        "dpkg -l alsa-utils",
        "amixer -c HID sget Mic",
        "amixer -c HID sget PCM",
        "interkom_asound_by_name_count",
        "interkom_amixer_pct",
        r#"interkom_mic_pct "$NAME_UPPER""#,
        r#"interkom_pcm_pct "$NAME_UPPER""#,
    ] {
        assert!(
            live_flow.contains(needle),
            "the (aa) live flow must call/read `{needle}` (#782) -- not just define it"
        );
    }
}

/// The (aa) block must be inserted BEFORE the (q) block so (q) stays the LAST check (the
/// provisioning-scripts.md (q)-last invariant + the check_q_is_wired test that slices to EOF).
#[test]
fn check_aa_is_inserted_before_q() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let live_flow = &body[guard_pos..];
    let aa = live_flow
        .find("# (aa) interkom audio bake-in")
        .expect("(aa) implementation block");
    let q = live_flow
        .rfind("# (q) .bak cruft drift")
        .expect("(q) implementation block");
    assert!(
        aa < q,
        "the (aa) check block must precede the (q) block so (q) remains the last check (aa={aa} q={q})"
    );
}

/// The (aa) check is a HARD gate on every drift facet: it must FAIL (not warn) on a non-by-NAME
/// asound.conf, a missing alsa-utils, an unreadable gain, and a per-box gain mismatch.
#[test]
fn check_aa_fails_on_each_drift_facet() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let live_flow = &body[guard_pos..];
    let aa = live_flow
        .find("# (aa) interkom audio bake-in")
        .expect("(aa) block start");
    // Scope the slice to the (aa) block ALONE — end it at the NEXT check block (ab), not at (q).
    // A later WARN-using check inserted between (aa) and (q) (e.g. the issue-899 (ac) realtime-
    // isolation drift check, which is WARN-only by design) must not be folded into this
    // (aa)-is-a-hard-gate assertion.
    let aa_end = live_flow[aa..]
        .find("\n# (ab) ")
        .map(|i| aa + i)
        .expect("(ab) block start (the check immediately after (aa))");
    let aa_block = &live_flow[aa..aa_end];
    assert!(
        aa_block.contains("is not the by-NAME form"),
        "(aa) must FAIL on a non-by-NAME asound.conf"
    );
    assert!(
        aa_block.contains("alsa-utils not installed"),
        "(aa) must FAIL when alsa-utils is missing"
    );
    assert!(
        aa_block.contains("interkom mixer gain drift"),
        "(aa) must FAIL on a per-box Mic/PCM gain mismatch"
    );
    // Every branch of (aa) that is not the OK branch is a fail(), never a warn().
    assert!(
        !aa_block.contains("warn \""),
        "(aa) is a hard acceptance gate -- it must never merely warn() on a drift. block: {aa_block:?}"
    );
    assert!(
        aa_block.contains("fail \""),
        "(aa) must call fail() on a drift"
    );
}

/// The (aa) check must be documented in BOTH the top-of-file header Checks list AND the usage()
/// Checks block (the provisioning-scripts.md "document in all THREE places" rule; the third place
/// is the executable block asserted above).
#[test]
fn aa_documented_in_header_and_usage() {
    let body = std::fs::read_to_string(script()).unwrap();
    let count = body.matches("(aa) interkom audio bake-in").count();
    assert!(
        count >= 3,
        "the (aa) check must appear in the header Checks list, the usage() Checks block, AND the \
         executable block (found {count} occurrences of the marker)"
    );
}

/// COMPOSITION: sourcing the REAL verify-device.sh actually WIRES the lib in -- the per-box table
/// resolves through the sourced function (proves the source line is live, not just a comment).
#[test]
fn verify_device_wires_per_box_gain_table() {
    let (code, out, err) = run_sourced(
        r#"printf '%s %s / %s %s\n' \
             "$(interkom_mic_pct CAM4)" "$(interkom_pcm_pct CAM4)" \
             "$(interkom_mic_pct CAM7)" "$(interkom_pcm_pct CAM7)""#,
    );
    assert_eq!(
        code, 0,
        "sourcing verify-device.sh must expose the lib functions. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "75 79 / 80 94",
        "cam1-4 = Mic 75/PCM 79, cam5-7 = Mic 80/PCM 94 (owner's per-box table)"
    );
}

// ---------------------------------------------------------------------------------------------
// (ad) provisioning netplan interface pin -- no USB-camera-link IP theft (#1155)
// ---------------------------------------------------------------------------------------------

/// Single-quote an arbitrary string for a bash argument (handles embedded single quotes), so a
/// multi-line netplan / `ip -br addr` fixture passes through `run_sourced`'s bash `-c` intact.
fn netplan_sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A wildcard netplan -- the #1155 regression signature. `match: driver: "*"` claims a USB
/// CDC-NCM camera link and hands it the box IP + a duplicate default route.
const NETPLAN_WILDCARD: &str = "network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        driver: \"*\"
      addresses:
        - 10.77.9.61/23
      routes:
        - to: default
          via: 10.77.8.1
";

/// The pinned netplan -- `match: name: \"enp*\"` matches only the PCI NIC, so a USB camera link
/// (enx*/cdc_ncm) stays unmanaged by the LAN stanza.
const NETPLAN_PINNED: &str = "network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        name: \"enp*\"
      addresses:
        - 10.77.9.61/23
      routes:
        - to: default
          via: 10.77.8.1
";

fn wildcard_count(text: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "netplan_driver_wildcard_count {}",
        netplan_sq(text)
    ));
    assert_eq!(
        code, 0,
        "netplan_driver_wildcard_count must exit 0 (pipefail-safe). stderr: {err}"
    );
    out.trim().to_string()
}

fn dup_ip_count(ipbrief: &str, boxip: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "interfaces_sharing_ip {} {}",
        netplan_sq(ipbrief),
        netplan_sq(boxip)
    ));
    assert_eq!(
        code, 0,
        "interfaces_sharing_ip must exit 0 (pipefail-safe). stderr: {err}"
    );
    out.trim().to_string()
}

/// The wildcard detector flags ONLY a `driver: "*"` LAN stanza (quoted, single-quoted or bare),
/// and returns "0" for the correctly pinned `name: "enp*"` stanza -- so the (ad) check FAILs the
/// old wildcard config and PASSes the fix.
#[test]
fn netplan_driver_wildcard_count_flags_the_1155_regression_only() {
    assert_eq!(
        wildcard_count(NETPLAN_WILDCARD),
        "1",
        "a `driver: \"*\"` LAN stanza is the #1155 regression signature"
    );
    assert_eq!(
        wildcard_count(NETPLAN_PINNED),
        "0",
        "a `name: \"enp*\"` pinned stanza must NOT count as the wildcard regression"
    );
    assert_eq!(
        wildcard_count("      match:\n        driver: '*'\n"),
        "1",
        "single-quoted `driver: '*'` is the same regression"
    );
    assert_eq!(
        wildcard_count("      match:\n        driver: *\n"),
        "1",
        "unquoted `driver: *` is the same regression"
    );
    assert_eq!(
        wildcard_count(""),
        "0",
        "empty input (unreachable box) -> 0, never an error"
    );
}

/// The duplicate-IP detector counts DISTINCT interfaces carrying the box IP, is CIDR-anchored
/// (so .61 never substring-matches .610), and returns 2 when a USB camera link has stolen the box
/// IP alongside the real NIC -- the live #1155 signature.
#[test]
fn interfaces_sharing_ip_counts_distinct_links_and_is_cidr_anchored() {
    let one = "lo               UNKNOWN        127.0.0.1/8 ::1/128\n\
               enp3s0           UP             10.77.9.61/23 fe80::1/64\n";
    let two = "lo               UNKNOWN        127.0.0.1/8 ::1/128\n\
               enp3s0           UP             10.77.9.61/23 fe80::1/64\n\
               enx02743ba02a02  UP             10.77.9.61/23 fe80::2/64\n";
    let none = "lo               UNKNOWN        127.0.0.1/8 ::1/128\n\
                enp3s0           UP             10.77.9.99/23 fe80::1/64\n";
    let longer = "lo               UNKNOWN        127.0.0.1/8\n\
                  enp3s0           UP             10.77.9.610/23\n";
    assert_eq!(
        dup_ip_count(one, "10.77.9.61"),
        "1",
        "healthy: box IP on one link"
    );
    assert_eq!(
        dup_ip_count(two, "10.77.9.61"),
        "2",
        "#1155 trap: box IP on the NIC AND a USB camera link"
    );
    assert_eq!(dup_ip_count(none, "10.77.9.61"), "0", "box IP absent -> 0");
    assert_eq!(
        dup_ip_count(longer, "10.77.9.61"),
        "0",
        "CIDR-anchored: .61 must not substring-match .610"
    );
    assert_eq!(
        dup_ip_count("", "10.77.9.61"),
        "0",
        "empty input (unreachable) -> 0, never an error"
    );
}

/// issue 1187 — verify-device.sh must ALSO acceptance-check that mpv is present + runnable (the
/// DRM/KMS lipsync playback runtime that replaced the legacy raw-fbdev ffmpeg write), mirroring the
/// (x) ffmpeg check. It must be inserted BEFORE the (q) block so (q) stays the intentionally-LAST
/// check, and it must FAIL loud (never merely warn) when mpv is missing.
#[test]
fn verify_device_checks_mpv_present_before_q_1187() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let live_flow = &body[guard_pos..];
    let mpv_at = live_flow
        .find("# (x2) mpv installed")
        .expect("(x2) mpv acceptance-check block must be present in the live flow");
    let q_at = live_flow
        .rfind("# (q) .bak cruft drift")
        .expect("(q) implementation block");
    assert!(
        mpv_at < q_at,
        "the mpv (x2) check must precede the (q) block so (q) stays last (mpv={mpv_at} q={q_at})"
    );
    // Scope the slice to the (x2) block ALONE -- it is inserted between (x) ffmpeg and (y), so it
    // ends at the (y) block that immediately follows it.
    let mpv_end = live_flow[mpv_at..]
        .find("\n# (y) ")
        .map(|i| mpv_at + i)
        .expect("(y) block start (the check immediately after the (x2) mpv check)");
    let mpv_block = &live_flow[mpv_at..mpv_end];
    assert!(
        mpv_block.contains("mpv --version"),
        "(x2) must actually probe mpv via `mpv --version`: {mpv_block}"
    );
    assert!(
        mpv_block.contains("fail "),
        "(x2) must FAIL loud (never merely warn) when mpv is missing/unrunnable: {mpv_block}"
    );
}

/// issue 1213 -- `v4l2-ctl` (package `v4l-utils`) is listed in setup-device.sh's STEP 16 apt-get
/// install line but was found MISSING on cam3/cam4 live: the whole install line is guarded by
/// `2>/dev/null || true`, so a per-box apt failure is swallowed silently and nothing downstream
/// ever checks the tool actually landed. `verify-device.sh` must acceptance-check `v4l2-ctl` the
/// same way it already does for ffmpeg (x) / mpv (x2) -- present AND runnable, not just that apt
/// didn't error -- inserted BEFORE the (q) block so (q) stays the intentionally-LAST check, and it
/// must FAIL loud (never merely warn) naming the missing package when the tool is absent.
#[test]
fn verify_device_checks_v4l2_ctl_present_before_q_1213() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let live_flow = &body[guard_pos..];
    let af_at = live_flow
        .find("# (af) v4l2-ctl installed")
        .expect("(af) v4l2-ctl acceptance-check block must be present in the live flow");
    let q_at = live_flow
        .rfind("# (q) .bak cruft drift")
        .expect("(q) implementation block");
    assert!(
        af_at < q_at,
        "the v4l2-ctl (af) check must precede the (q) block so (q) stays last (af={af_at} q={q_at})"
    );
    // Scope the slice to the (af) block ALONE -- it is inserted directly before (q), with no other
    // lettered check between them, so the block simply runs up to the (q) marker itself.
    let af_block = &live_flow[af_at..q_at];
    assert!(
        af_block.contains("v4l2-ctl --version"),
        "(af) must actually probe v4l2-ctl via `v4l2-ctl --version`: {af_block}"
    );
    assert!(
        af_block.contains("fail "),
        "(af) must FAIL loud (never merely warn) when v4l2-ctl is missing/unrunnable: {af_block}"
    );
    assert!(
        af_block.contains("v4l-utils"),
        "(af)'s FAIL message must name the missing package (v4l-utils) by name -- \
never a measured zero (.claude/rules/imag-ssh-remote-tool-preflight.md): {af_block}"
    );
}

/// Companion to the live-flow test above: the (af) letter must also be documented in the
/// top-of-file header "Checks (all must pass)" list AND in `usage()`'s own Checks doc block --
/// the THREE-place documentation convention `.claude/rules/provisioning-scripts.md` establishes
/// for every new check letter.
#[test]
fn verify_device_documents_af_in_header_and_usage_1213() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let header = &body[..guard_pos];
    assert!(
        header.contains("(af)") && header.contains("v4l2-ctl"),
        "the top-of-file header Checks list must document (af) v4l2-ctl -- header text: {header}"
    );
    let usage_start = body.find("usage() {").expect("usage() function definition");
    let usage_end = body[usage_start..]
        .find("\nEOF\n")
        .map(|i| usage_start + i)
        .expect("usage() heredoc terminator");
    let usage_block = &body[usage_start..usage_end];
    assert!(
        usage_block.contains("(af)") && usage_block.contains("v4l2-ctl"),
        "usage()'s own Checks doc block must document (af) v4l2-ctl: {usage_block}"
    );
}

/// issue 1240 -- `ethtool` is listed in setup-device.sh's STEP 16 apt-get install line (same
/// silently-swallowed `2>/dev/null || true` class as (af) v4l2-ctl, issue 1213), but nothing ever
/// acceptance-checks it landed. Worse than v4l2-ctl: setup-device.sh's OWN EEE/flow-control
/// tuning (`ethtool --set-eee` / `ethtool -A`, STEP 2) is ALSO `|| true`-guarded, so a box missing
/// ethtool silently ran with NO NIC tuning at all -- cam3 was found live missing it entirely,
/// having run for years with the optimize-nic hook a permanent silent no-op. `verify-device.sh`
/// must acceptance-check `ethtool` the same minimal way it already does for fuser (t) -- present
/// on PATH via `command -v` -- inserted BEFORE the (q) block so (q) stays the intentionally-LAST
/// check, and it must FAIL loud (never merely warn) naming the missing package when absent.
#[test]
fn verify_device_checks_ethtool_present_before_q_1240() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let live_flow = &body[guard_pos..];
    let ag_at = live_flow
        .find("# (ag) ethtool installed")
        .expect("(ag) ethtool acceptance-check block must be present in the live flow");
    let q_at = live_flow
        .rfind("# (q) .bak cruft drift")
        .expect("(q) implementation block");
    assert!(
        ag_at < q_at,
        "the ethtool (ag) check must precede the (q) block so (q) stays last (ag={ag_at} q={q_at})"
    );
    // Scope the slice to the (ag) block ALONE -- it is inserted directly before (q), with no other
    // lettered check between them, so the block simply runs up to the (q) marker itself.
    let ag_block = &live_flow[ag_at..q_at];
    assert!(
        ag_block.contains("command -v ethtool"),
        "(ag) must actually probe ethtool via `command -v ethtool`: {ag_block}"
    );
    assert!(
        ag_block.contains("fail "),
        "(ag) must FAIL loud (never merely warn) when ethtool is missing: {ag_block}"
    );
    assert!(
        ag_block.contains("ethtool"),
        "(ag)'s FAIL message must name the missing package (ethtool) by name -- \
never a measured zero (.claude/rules/imag-ssh-remote-tool-preflight.md): {ag_block}"
    );
}

/// Companion to the live-flow test above: the (ag) letter must also be documented in the
/// top-of-file header "Checks (all must pass)" list AND in `usage()`'s own Checks doc block --
/// the THREE-place documentation convention `.claude/rules/provisioning-scripts.md` establishes
/// for every new check letter.
#[test]
fn verify_device_documents_ag_in_header_and_usage_1240() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_pos = body
        .find("never run the live SSH flow below.")
        .expect("source-guard comment");
    let header = &body[..guard_pos];
    assert!(
        header.contains("(ag)") && header.contains("ethtool"),
        "the top-of-file header Checks list must document (ag) ethtool -- header text: {header}"
    );
    let usage_start = body.find("usage() {").expect("usage() function definition");
    let usage_end = body[usage_start..]
        .find("\nEOF\n")
        .map(|i| usage_start + i)
        .expect("usage() heredoc terminator");
    let usage_block = &body[usage_start..usage_end];
    assert!(
        usage_block.contains("(ag)") && usage_block.contains("ethtool"),
        "usage()'s own Checks doc block must document (ag) ethtool: {usage_block}"
    );
}
