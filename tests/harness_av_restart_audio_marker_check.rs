//! #421 — the `#137` AV_RESTART_GATE painter (recording-e2e.sh's
//! `av_restart_record_and_emit_plan()`) must AUDIBLY verify its own QPSK marker is actually
//! producing sound before proceeding to record — the SAME risk class `#420` already fixed for
//! `scripts/rig-mode.sh`'s TEST-mode painter launch.
//!
//! ## The bug
//!
//! `av_restart_record_and_emit_plan()` launches cam2's painter with `--audio-marker
//! --audio-marker-device ... --audio-marker-cadence-ticks ... --marker-log ...` but never checks
//! that the marker's ALSA PCM actually came up. If a flag were ever dropped/mistyped, or the
//! device were busy, this mode would silently record an unmeasured before/after pair — exactly
//! the #420 failure class, just in a different file.
//!
//! ## The fix (what these tests lock)
//!
//! The `#420` audible self-check (parse `hw:CARD=<id>,DEV=<n>` -> `/proc/asound/<id>/pcm<n>p/
//! sub0/status`, poll for `state: RUNNING`, fail loud otherwise) is extracted out of
//! `scripts/rig-mode.sh` into a shared, source-only helper `scripts/lib/audio-marker-check.sh`
//! (mirrors the `#309` `scripts/lib/rig-test-dropin.sh` shape exactly: pure string builders, no
//! ssh, no side effects at source time). Both `rig-mode.sh` (the original #420 fix) and
//! `recording-e2e.sh` (`#421`) source it and call `audio_marker_check_cmds`, so the two painter
//! launches can never drift on what "audible" means.
//!
//! Same PURE-STRING model as `tests/rig_mode.rs` / `tests/harness_rig_test_dropin_cleanup.rs` /
//! `tests/harness_av_restart_sync_gate.rs`: source the real helper and inspect its emitted text,
//! or read the real orchestration scripts and assert on their source — no ssh to a live rig.
//!
//! ## #431 hardening — RUNNING is not audible
//!
//! The QPSK emitter (`src/probe/qpsk_emit.rs`) is a CONTINUOUS-FEED design: it ALWAYS writes to
//! the ALSA ring — silence between markers, marker samples when one is due — precisely so the PCM
//! never underruns. So the `#420` check above (`state: RUNNING`) is satisfied by the silence
//! carrier ALONE, even if the painter's refresh tick stalls and ZERO discrete markers ever fire.
//! `audio_marker_emission_check_cmds` closes that gap by polling the marker-log CSV the emitter
//! now appends to PER EMITTED MARKER (not just on shutdown) and failing loud if it never grows.
//! Unlike the RUNNING check (which needs a real `/proc/asound` path this harness can't fake), the
//! emission check depends on nothing but a file — so its tests EXECUTE the generated snippet for
//! real against a controlled fixture, proving it can actually FAIL and PASS on the same mechanism.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const LIB_REL: &str = "scripts/lib/audio-marker-check.sh";

/// Source ONLY the shared helper (no orchestration script) and run `body`, returning stdout.
/// Asserts the harness itself exited 0 — the pure builders never fail.
fn run_sourced(body: &str) -> String {
    let lib = manifest_dir().join(LIB_REL);
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "#421: sourced harness exited non-zero (lib={}).\nstdout={:?}\nstderr={:?}",
        lib.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The shared helper must exist as its own source-only file (DRY-extracted out of rig-mode.sh).
#[test]
fn shared_audio_marker_check_lib_exists() {
    let lib = manifest_dir().join(LIB_REL);
    assert!(
        lib.exists(),
        "#421: {} must exist — the #420 audible self-check must be extracted into a shared \
         helper, not stay inline-only in scripts/rig-mode.sh",
        lib.display()
    );
}

/// `audio_marker_alsa_status_path` is the pure CARD/DEV parser: given a `hw:CARD=<id>,DEV=<n>`
/// ALSA device string it must derive exactly `/proc/asound/<id>/pcm<n>p/sub0/status` — the real
/// kernel-reported PCM status path #420 keys its RUNNING check on.
#[test]
fn alsa_status_path_derived_from_device_string() {
    let out = run_sourced(r#"audio_marker_alsa_status_path "hw:CARD=PCH,DEV=3""#);
    assert_eq!(
        out.trim(),
        "/proc/asound/PCH/pcm3p/sub0/status",
        "#421: default cam2 device must resolve to the PCH/pcm3p status path. Got:\n{out}"
    );

    let out2 = run_sourced(r#"audio_marker_alsa_status_path "hw:CARD=USB,DEV=0""#);
    assert_eq!(
        out2.trim(),
        "/proc/asound/USB/pcm0p/sub0/status",
        "#421: an overridden device must resolve to ITS OWN status path, not a hardcoded \
         default. Got:\n{out2}"
    );
}

/// The headline behavioral guard: `audio_marker_check_cmds` must emit a REMOTE bash snippet that
/// polls the derived ALSA status path for `state: RUNNING` and FAILS LOUD (never a silent
/// pass-through) — identifying itself as the `#420` check — when the marker never comes up.
#[test]
fn check_cmds_poll_running_state_and_fail_loud_when_silent() {
    let p = run_sourced(
        r#"audio_marker_check_cmds "hw:CARD=PCH,DEV=3" 'pkill -x frame-probe 2>/dev/null || true' "cadence=180 ticks""#,
    );
    assert!(
        p.contains("/proc/asound/PCH/pcm3p/sub0/status"),
        "#421: the self-check must read the REAL ALSA PCM status file — a genuine kernel \
         signal, never a stub. Got:\n{p}"
    );
    assert!(
        p.contains("state: RUNNING"),
        "#421: the self-check must assert the PCM is in the RUNNING state (actively streaming, \
         not merely opened/prepared). Got:\n{p}"
    );
    assert!(
        p.contains("is NOT RUNNING") && p.contains("#420"),
        "#421: a silent marker must FAIL LOUD identifying itself as the #420-class check (never \
         a silent pass-through). Got:\n{p}"
    );
    // The caller-supplied teardown (2nd arg) must run in the FAIL branch, before the exit.
    let fail_pos = p
        .find("is NOT RUNNING")
        .expect("#421: expected the silent-marker FAIL branch");
    let cleanup_pos = p[fail_pos..]
        .find("pkill -x frame-probe")
        .map(|i| i + fail_pos)
        .expect(
            "#421: the caller-supplied cleanup command must run in the FAIL branch (a silent \
             marker leaves a stray, unmeasured painter process behind otherwise)",
        );
    let exit_pos = p[cleanup_pos..]
        .find("exit 1")
        .map(|i| i + cleanup_pos)
        .expect("#421: the FAIL branch must exit non-zero AFTER running the cleanup command");
    assert!(cleanup_pos > fail_pos && exit_pos > cleanup_pos);
}

/// `recording-e2e.sh` must source the shared helper (mirrors the `#309` single-source guard —
/// never re-derive the ALSA CARD/DEV parsing independently).
#[test]
fn recording_e2e_sources_the_shared_audio_marker_check_lib() {
    assert!(
        read("scripts/recording-e2e.sh").contains("audio-marker-check.sh"),
        "#421: scripts/recording-e2e.sh must source scripts/lib/audio-marker-check.sh (single \
         source of truth shared with rig-mode.sh's #420 fix)"
    );
}

/// `rig-mode.sh` must ALSO source the shared helper post-refactor — the DRY extraction moved the
/// #420 logic OUT of rig-mode.sh's inline heredoc, not just copy-pasted it into a second file.
#[test]
fn rig_mode_sources_the_shared_audio_marker_check_lib() {
    assert!(
        read("scripts/rig-mode.sh").contains("audio-marker-check.sh"),
        "#421: scripts/rig-mode.sh must source scripts/lib/audio-marker-check.sh (DRY-extracted \
         out of its own inline #420 self-check, not duplicated)"
    );
}

/// The headline reachability guard: the AV_RESTART_GATE block (which defines and calls
/// `av_restart_record_and_emit_plan`) must WIRE the shared self-check, and the call must land
/// AFTER the cam2 painter is launched but BEFORE the gate starts OBS recording — a self-check
/// that ran after recording already started would be too late to prevent an unmeasured run.
#[test]
fn av_restart_painter_calls_the_shared_self_check_before_recording_starts() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("AV_RESTART_GATE:-0")
        .expect("#137 AV_RESTART_GATE block must exist");
    let start_record_pos = s
        .find("[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    let block = &s[gate_pos..start_record_pos];

    let launch_pos = block
        .find("--audio-marker-device $AV_RESTART_MARKER_DEVICE")
        .expect("#421: expected the existing AV_RESTART_GATE painter launch with --audio-marker");
    // Search for the actual CALL syntax `$(audio_marker_check_cmds` (a command substitution),
    // not the bare function name — a prose comment mentioning the helper by name must not
    // false-satisfy this guard.
    let check_pos = block.find("$(audio_marker_check_cmds").expect(
        "#421: av_restart_record_and_emit_plan() must CALL the shared audio_marker_check_cmds \
         self-check (same risk class as #420 — a dropped/mistyped flag or a busy ALSA device \
         must abort the AV_RESTART_GATE run, never silently record an unmeasured pair)",
    );
    assert!(
        check_pos > launch_pos,
        "#421: the self-check must run AFTER the painter launch. Got block:\n{block}"
    );

    // The self-check call must be inside av_restart_record_and_emit_plan() itself (before the
    // function's closing brace), i.e. it gates BOTH the 'before' and 'after' recordings — not a
    // one-off check bolted on only at the call site.
    let record_start_local = block
        .find("python3 \"$HERE/obs_phase2.py\" record")
        .expect("#421: expected the OBS StartRecord call inside av_restart_record_and_emit_plan");
    assert!(
        check_pos < record_start_local,
        "#421: the self-check must run BEFORE the gate starts OBS recording — a check that \
         fires after recording already started is too late to prevent an unmeasured run. \
         Got block:\n{block}"
    );
}

// -------------------------------------------------------------------------------------------
// #431 — emission-assert tests (see the module doc for why these EXECUTE the real snippet)
// -------------------------------------------------------------------------------------------

fn emission_scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "audio-marker-emit-431-{}-{}",
        std::process::id(),
        name
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Source the shared helper, build the `audio_marker_emission_check_cmds LOG ON_FAIL EXTRA`
/// snippet, then `eval` it in the SAME bash process — with the poll interval/attempts shrunk via
/// env vars (the production defaults are ~3s x 3 ≈ 9s) so the test runs in a couple of seconds.
/// Returns (exit_code, stdout, stderr).
fn exec_emission_check(
    log: &Path,
    on_fail_cmd: &str,
    extra: &str,
    interval_secs: u64,
    attempts: u64,
) -> (i32, String, String) {
    let lib = manifest_dir().join(LIB_REL);
    let script = format!(
        r#"set -uo pipefail
. "{lib}"
snippet="$(audio_marker_emission_check_cmds "{log}" '{on_fail_cmd}' "{extra}")"
eval "$snippet"
"#,
        lib = lib.display(),
        log = log.display(),
        on_fail_cmd = on_fail_cmd,
        extra = extra,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env(
            "AUDIO_MARKER_EMIT_POLL_INTERVAL_SECS",
            interval_secs.to_string(),
        )
        .env("AUDIO_MARKER_EMIT_POLL_ATTEMPTS", attempts.to_string())
        .output()
        .expect("#431: failed to run the emission-check harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// HEADLINE (#431): a marker log whose row count NEVER GROWS across the whole poll window must
/// FAIL LOUD (non-zero exit), identify itself as #431, explain that RUNNING-but-silent is not
/// audible, and run the caller-supplied cleanup (`on_fail_cmd`) before exiting — the same
/// fail-fast contract the #420 RUNNING check already has, now extended to real emission. This
/// EXECUTES the generated snippet against a real static fixture file — not a string-content
/// check — so it proves the assertion can genuinely fail on a silent/stalled emitter.
#[test]
fn emission_check_fails_loud_when_marker_log_never_grows() {
    let dir = emission_scratch("stall");
    let log = dir.join("markers.csv");
    let sentinel = dir.join("cleaned-up");
    fs::write(
        &log,
        "# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n0,100,1000\n",
    )
    .unwrap();

    let (code, _out, err) = exec_emission_check(
        &log,
        &format!("touch {}", sentinel.display()),
        "test=stall",
        1,
        2,
    );

    assert_eq!(
        code, 1,
        "#431: a marker log that never grows must FAIL LOUD (exit 1). stderr:\n{err}"
    );
    assert!(
        err.contains("#431") && err.to_lowercase().contains("not grown"),
        "#431: the failure must identify itself and explain the emission is stalled. stderr:\n{err}"
    );
    assert!(
        sentinel.exists(),
        "#431: the caller-supplied cleanup (on_fail_cmd) must run when emission is stalled — a \
         silent painter must not be left running unmeasured"
    );
}

/// HEADLINE (#431): the moment the marker log's row count grows (a real marker fired), the check
/// must PASS (exit 0) — proving this is a REAL assertion that can both fail AND succeed on the
/// same underlying mechanism, never a tautology. A background thread appends a row shortly after
/// polling starts so the check's next poll observes growth.
#[test]
fn emission_check_passes_once_marker_log_grows() {
    let dir = emission_scratch("grow");
    let log = dir.join("markers.csv");
    fs::write(
        &log,
        "# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n0,100,1000\n",
    )
    .unwrap();

    let log_for_writer = log.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_for_writer)
            .expect("#431: append fixture marker row");
        writeln!(f, "1,101,2000000").unwrap();
    });

    let (code, out, err) = exec_emission_check(&log, "true", "test=grow", 1, 4);
    writer.join().unwrap();

    assert_eq!(
        code, 0,
        "#431: a marker log that grows mid-poll must PASS (exit 0). stdout={out:?} stderr={err:?}"
    );
    assert!(
        out.contains("PASS") && out.contains("#431"),
        "#431: a successful emission check must print a PASS line identifying itself. stdout:\n{out}"
    );
}

/// `audio_marker_check_cmds` (the combined #420 RUNNING + #431 emission check) must APPEND the
/// emission-check block ONLY when given a 4th MARKER_LOG_PATH argument — a 3-arg call (existing
/// callers, and the string-content tests above) must see byte-identical output to before #431.
#[test]
fn combined_check_cmds_appends_emission_block_only_when_marker_log_given() {
    let without_log = run_sourced(r#"audio_marker_check_cmds "hw:CARD=PCH,DEV=3" 'true' "ctx""#);
    assert!(
        !without_log.contains("#431"),
        "#431: with no marker-log argument, the emission check must be skipped entirely. Got:\n{without_log}"
    );

    let with_log = run_sourced(
        r#"audio_marker_check_cmds "hw:CARD=PCH,DEV=3" 'true' "ctx" "/tmp/markers.csv""#,
    );
    assert!(
        with_log.contains("#431") && with_log.contains("/tmp/markers.csv"),
        "#431: with a marker-log argument, the emission-check block must be appended, keyed on \
         that exact path. Got:\n{with_log}"
    );
    // The emission check must come AFTER the RUNNING PASS line (it only makes sense once the PCM
    // is confirmed RUNNING).
    let running_pass = with_log
        .find("PASS: #420")
        .expect("#431: expected the existing #420 RUNNING PASS line to still be present");
    let emission_pos = with_log
        .find("#431")
        .expect("#431: expected the emission-check block");
    assert!(
        emission_pos > running_pass,
        "#431: the emission check must run AFTER the RUNNING check passes. Got:\n{with_log}"
    );
}

/// #667 REGRESSION: `painter_launch_remote`'s WHOLE heredoc runs under `set -e` (rig-mode.sh puts
/// `set -e` at its top, scripts/rig-mode.sh:208) — but `grep -c PATTERN FILE` returns a NON-ZERO
/// exit status whenever the match count is zero (or the file doesn't exist yet), even though it
/// correctly prints "0"/nothing to stdout. `c0=$(grep -c ...)` is a plain assignment whose exit
/// status IS grep's exit status, so under `set -e` this SILENTLY ABORTS THE WHOLE SCRIPT the
/// instant it samples the marker log before the very first QPSK marker has fired (~3s cadence
/// after painter launch) — with ZERO output, before this check ever gets to print its own #431
/// PASS/FAIL diagnostic. Live evidence: rig-mode.sh exited 1 right after the #420 RUNNING PASS
/// line ~4/5 runs, never reaching toggle_burn/enforce_strih_ndi_mapping/set_imag_test_program.
/// This test runs the REAL emission-check snippet under `set -e` (matching the production
/// heredoc) against a marker-log path that does not exist yet — the exact race window — and
/// asserts the check reaches ITS OWN diagnostic output instead of dying silently on grep's raw
/// exit code.
#[test]
fn emission_check_survives_set_e_when_marker_log_not_yet_created_667() {
    let dir = emission_scratch("667-sete-no-file");
    // Deliberately never created before the check runs — reproduces the exact race: the marker
    // log doesn't exist yet (or has zero rows) at the moment `painter_launch_remote` samples it.
    let log = dir.join("markers-not-yet-created.csv");
    let lib = manifest_dir().join(LIB_REL);
    let script = format!(
        r#"set -e
. "{lib}"
snippet="$(audio_marker_emission_check_cmds "{log}" 'true' "test=667")"
eval "$snippet"
"#,
        lib = lib.display(),
        log = log.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("AUDIO_MARKER_EMIT_POLL_INTERVAL_SECS", "1")
        .env("AUDIO_MARKER_EMIT_POLL_ATTEMPTS", "1")
        .output()
        .expect("#667: failed to run the set -e harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("#431") || stderr.contains("#431"),
        "#667: under `set -e` (the real remote heredoc's own mode), the emission check must reach \
         its own #431 diagnostic (PASS or FAIL) instead of dying silently on grep -c's own exit \
         status the moment the marker log doesn't exist yet — the exact race rig-mode.sh hit ~4/5 \
         runs, exiting right after the #420 RUNNING PASS line with zero further output. \
         exit={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
}

/// Both real call sites must pass their known marker-log path as the 4th positional argument, so
/// the #431 hardening actually applies in production — not merely available-but-unused.
#[test]
fn rig_mode_and_recording_e2e_pass_marker_log_to_the_check() {
    let rig_mode = read("scripts/rig-mode.sh");
    let check_line = rig_mode
        .lines()
        .find(|l| l.contains("$(audio_marker_check_cmds"))
        .expect("#431: expected the audio_marker_check_cmds call in rig-mode.sh");
    assert!(
        check_line.contains("\"$marker_log\""),
        "#431: rig-mode.sh's call must pass $marker_log as the 4th arg so the emission check has \
         a real path to poll. Got:\n{check_line}"
    );

    let recording_e2e = read("scripts/recording-e2e.sh");
    let block_pos = recording_e2e
        .find("AV_RESTART_GATE:-0")
        .expect("#137 AV_RESTART_GATE block must exist");
    let check_pos = recording_e2e[block_pos..]
        .find("$(audio_marker_check_cmds")
        .map(|i| i + block_pos)
        .expect("#431: expected the audio_marker_check_cmds call in the AV_RESTART_GATE block");
    let check_line_e2e = recording_e2e[check_pos..]
        .lines()
        .next()
        .unwrap_or_default();
    assert!(
        check_line_e2e.contains("av-restart-markers.csv"),
        "#431: recording-e2e.sh's AV_RESTART_GATE call must pass the painter's own \
         /tmp/av-restart-markers.csv as the 4th arg. Got:\n{check_line_e2e}"
    );
}
