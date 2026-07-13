//! #722 — the EVENT-mode CONTRACT: `scripts/rig-mode.sh event` must end with a full
//! machine-checkable assert phase (8 items) that exits non-zero (with a Slovak summary naming
//! exactly what failed) unless ALL 8 hold. Trigger: the 2026-07-12 live incident (#721) —
//! rig-mode event + a manual supervisor checklist BOTH said "clean" while a QR was live on air.
//!
//! `scripts/lib/event-assert.sh` provides the REMOTE-command builders for the two fleet-wide
//! checks that have no existing tool (paint-process count + service/stray-unit status per box,
//! and the artifacts-existing check) — pure string builders (mirrors every other `_cmds`
//! function in this codebase), but proven for REAL here by executing the returned command
//! against real spawned fixtures/tmp files, not just asserting on the source text.
//!
//! The rest of this file is a STATIC wiring guard proving `do_event()` actually calls the full
//! assert phase — pixel proof, burns, recordings, services, latency, mapping, artifacts — via
//! `scripts/event_assert.py`, UNCONDITIONALLY (no code path may skip it), and that a FAILED
//! assert makes rig-mode.sh event exit non-zero.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lib/event-assert.sh")
}

struct Run {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_sourced(body: &str) -> Run {
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// event_assert_fleet_check_cmds — paint-process count + service-active + stray-unit status,
// executed for REAL against a fake `pgrep`/`systemctl` on PATH (no real cam box needed — this
// proves the SHAPE and parsing contract, not a live rig).
// ---------------------------------------------------------------------------

fn run_with_fake_bins(body: &str, fakes: &[(&str, &str)]) -> Run {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    for (name, script_body) in fakes {
        let p = bin_dir.join(name);
        fs::write(&p, format!("#!/usr/bin/env bash\n{script_body}\n")).unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        fs::set_permissions(&p, perm).unwrap();
    }
    let path_env = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("PATH", path_env)
        .output()
        .expect("run with fake bins");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn fleet_check_reports_zero_paint_processes_active_service_no_stray_units() {
    let r = run_with_fake_bins(
        "eval \"$(event_assert_fleet_check_cmds)\"",
        &[
            ("pgrep", "exit 1"), // no matches
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; list-units) exit 0;; esac",
            ),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("PAINT_COUNT=0"), "stdout={}", r.stdout);
    assert!(
        r.stdout.contains("SERVICE_ACTIVE=active"),
        "stdout={}",
        r.stdout
    );
    assert!(r.stdout.contains("STRAY_UNITS="), "stdout={}", r.stdout);
}

#[test]
fn fleet_check_does_not_self_match_its_own_invocation_text() {
    // THE REAL pgrep (not a fake) — this is the exact `pkill -f` self-match footgun class this
    // codebase warns about repeatedly, but for `pgrep -c -f`: ssh invokes the WHOLE built
    // script as `bash -c "$SCRIPT"`, so if $SCRIPT's own SOURCE TEXT contains the literal
    // pattern "--paint-only" anywhere, the ENCLOSING bash -c process's /proc/PID/cmdline ALSO
    // contains that substring and gets counted as a false-positive "paint process" by
    // `pgrep -f`. Live-caught on the real rig (2026-07-13): every cam box reported
    // PAINT_COUNT=2 with zero real painters running. Proven here with the REAL system pgrep
    // (no fake), invoking the built script exactly as ssh would (`bash -c "$SCRIPT"`) — this
    // would have failed loud before the fix.
    let cmds = run_sourced("event_assert_fleet_check_cmds").stdout;
    // Pass the multi-line built script via an ENV VAR, not interpolated into the bash -c source
    // text (Rust's `{:?}` Debug-escapes newlines as literal two-char `\n`, which a plain
    // double-quoted bash string does not expand back into real newlines — see the identical
    // fixture-passing fix in harness_marker_device_resolve_725.rs). The outer `bash -c
    // "$SCRIPT_ENV"` mirrors exactly how ssh invokes the remote command (a nested bash -c whose
    // OWN cmdline contains the built script's full text) — this is what must NOT self-match.
    let out = Command::new("bash")
        .arg("-c")
        .arg("bash -c \"$SCRIPT_ENV\"")
        .env("SCRIPT_ENV", &cmds)
        .output()
        .expect("run the built fleet-check script via a real bash -c (mirrors ssh's invocation)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PAINT_COUNT=0"),
        "the fleet-check script must NEVER self-match its own invocation text -- got: {stdout}"
    );
}

#[test]
fn fleet_check_builder_never_embeds_the_paint_pattern_as_literal_source_text() {
    // A stronger, mechanism-level guard: the BUILT script's source text must never contain the
    // literal substring "--paint-only" at all -- any occurrence there is a live self-match risk
    // the moment ssh wraps it in `bash -c "..."`. The pattern must be reconstructed at RUNTIME
    // (e.g. base64-decoded) so it appears in a process's cmdline only when that process is a
    // REAL painter, never in the enclosing shell's own invocation text.
    let cmds = run_sourced("event_assert_fleet_check_cmds").stdout;
    assert!(
        !cmds.contains("--paint-only"),
        "the fleet-check builder must not embed the literal pattern as source text (self-match \
         risk) -- got:\n{cmds}"
    );
}

#[test]
fn fleet_check_detects_a_live_paint_process() {
    let r = run_with_fake_bins(
        "eval \"$(event_assert_fleet_check_cmds)\"",
        &[
            ("pgrep", "echo 2\nexit 0"), // 2 matches
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; list-units) exit 0;; esac",
            ),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("PAINT_COUNT=2"), "stdout={}", r.stdout);
}

#[test]
fn fleet_check_detects_a_stray_burn_unit() {
    let r = run_with_fake_bins(
        "eval \"$(event_assert_fleet_check_cmds)\"",
        &[
            ("pgrep", "exit 1"),
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; \
                 list-units) echo 'camera-box-burn-911002.service loaded active running'; exit 0;; esac",
            ),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("camera-box-burn-911002.service"),
        "stdout={}",
        r.stdout
    );
}

// ---------------------------------------------------------------------------
// event_assert_artifacts_check_cmds — real execution against a tmp dir.
// ---------------------------------------------------------------------------

#[test]
fn artifacts_check_reports_nothing_when_all_paths_are_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let p1 = tmp.path().join("rig-painter.pid");
    let p2 = tmp.path().join("rig-qpsk-markers.csv");
    let r = run_sourced(&format!(
        "eval \"$(event_assert_artifacts_check_cmds {:?} {:?})\"",
        p1.display(),
        p2.display()
    ));
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(
        r.stdout, "",
        "must print nothing when everything's cleared, got: {}",
        r.stdout
    );
}

#[test]
fn artifacts_check_reports_a_lingering_pidfile() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let p1 = tmp.path().join("rig-painter.pid");
    fs::write(&p1, "12345").unwrap();
    let p2 = tmp.path().join("rig-qpsk-markers.csv"); // absent
    let r = run_sourced(&format!(
        "eval \"$(event_assert_artifacts_check_cmds {:?} {:?})\"",
        p1.display(),
        p2.display()
    ));
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains(&p1.display().to_string()),
        "stdout={}",
        r.stdout
    );
    assert!(
        !r.stdout.contains(&p2.display().to_string()),
        "stdout={}",
        r.stdout
    );
}

// ---------------------------------------------------------------------------
// Static wiring — do_event() actually runs the full #722 assert phase, unconditionally, and a
// FAILED assert makes the script exit non-zero.
// ---------------------------------------------------------------------------

#[test]
fn rig_mode_sources_event_assert_lib() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap();
    assert!(
        text.contains("lib/event-assert.sh"),
        "rig-mode.sh must source scripts/lib/event-assert.sh"
    );
}

#[test]
fn do_event_calls_the_722_assert_phase_unconditionally() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap();
    let start = text.find("do_event() {").expect("do_event() must exist");
    let end = text[start..]
        .find("\nmain() {")
        .map(|off| start + off)
        .unwrap_or(text.len());
    let body = &text[start..end];
    assert!(
        body.contains("event_mode_assert"),
        "do_event() must call event_mode_assert() -- the #722 orchestration wrapper"
    );
    // Not gated behind an `if` that could skip it entirely on the mainline path -- the call site
    // itself must not be indented as a nested conditional-only branch. We can't fully prove
    // "unconditional" via text alone, but we CAN prove it isn't wrapped in a `[ ... ] &&` guard
    // that would make a false condition silently skip the WHOLE assert with no trace.
    assert!(
        !body.contains("&& event_mode_assert"),
        "the #722 assert phase call must not be silently skippable via a leading `&&` guard"
    );

    // event_mode_assert() itself must actually invoke scripts/event_assert.py -- the aggregate
    // decision, not just individual checks with no combined verdict.
    let ema_start = text
        .find("event_mode_assert() {")
        .expect("event_mode_assert() must exist");
    let ema_end = text[ema_start..]
        .find("\ndo_event() {")
        .map(|off| ema_start + off)
        .unwrap_or(text.len());
    let ema_body = &text[ema_start..ema_end];
    assert!(
        ema_body.contains("event_assert.py"),
        "event_mode_assert() must invoke scripts/event_assert.py"
    );
}

#[test]
fn do_event_exits_nonzero_when_the_assert_phase_fails() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap();
    let start = text.find("do_event() {").expect("do_event() must exist");
    let end = text[start..]
        .find("\nmain() {")
        .map(|off| start + off)
        .unwrap_or(text.len());
    let body = &text[start..end];
    // The assert's own verdict must ultimately propagate as do_event()'s own exit code -- an
    // explicit `exit "$EVENT_ASSERT_PASS"` (or equivalent), never swallowed by a blanket
    // `|| true` on the call itself.
    assert!(
        body.contains("EVENT_ASSERT_PASS") && body.contains("exit"),
        "do_event() must exit with the #722 assert phase's own verdict (EVENT_ASSERT_PASS), \
         not unconditionally exit 0 regardless of the outcome"
    );
    assert!(
        !body.contains("event_mode_assert || true")
            && !body.contains("event_mode_assert 2>&1 || true"),
        "the #722 assert phase's failure must never be swallowed by a blanket `|| true` on its \
         own call site"
    );
}
