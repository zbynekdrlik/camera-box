//! issue 1260 — the on-strih and on-stream `recording-verdict.exe --extract-partial` decode
//! (launched by `recording-e2e.sh` `[8/8a]`/`[8/8b]` after every E2E run) competes with the LIVE
//! `obs64` process for CPU on the same box for ~20 minutes at Windows' default (Normal) process
//! priority, correlated in the strih OBS `multiview-audit` log with a rendered_fps collapse
//! (supervisor comment 5518052846: 0-3 dips/10min idle vs 89-117 dips/10min during exactly the
//! on-strih decode windows, program `lagged=0` throughout). issue 767 already solved the same
//! class for imag-nb (`nice -n 19` in `build_onimag_command`); this mirrors it for the two
//! Windows planners via a host-process `PriorityClass` set (the `&`-invoked child inherits it —
//! Win32 `CreateProcess` semantics), resolved by the shared, pure `onbox_decode_priority_class`
//! in `scripts/lib/win-ssh-exec.sh` and applied by `build_onbox_command` in BOTH
//! `scripts/recording-verdict-on-strih.sh` / `-on-stream.sh`.
//!
//! Sourced-and-called-directly style, mirroring `harness_recording_verdict_on_imag.rs` and the
//! existing `on_stream_planner_builds_a_valid_windows_command` (harness_recording_e2e_paths.rs) —
//! `build_onbox_command` is a pure string function, no network needed.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn strih_script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-strih.sh")
}
fn stream_script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-stream.sh")
}

/// Runs `. <script>; build_onbox_command "$exe" <args...>` with an optional
/// `E2E_ONBOX_DECODE_PRIORITY` override, returning (stdout, stderr, exit-success).
fn run_build_onbox_command(
    script: &PathBuf,
    exe: &str,
    args: &[&str],
    priority_env: Option<&str>,
) -> (String, String, bool) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(". \"$1\"; build_onbox_command \"$2\" \"${@:3}\"")
        .arg("bash") // $0
        .arg(script) // $1 — the script to source
        .arg(exe) // $2 — the exe
        .args(args); // $3.. — forwarded args
    match priority_env {
        Some(v) => {
            cmd.env("E2E_ONBOX_DECODE_PRIORITY", v);
        }
        None => {
            cmd.env_remove("E2E_ONBOX_DECODE_PRIORITY");
        }
    }
    let out = cmd.output().expect("run build_onbox_command");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

const EXE: &str = r"C:\camera-box\recording-verdict.exe";
// Same shape as the win_ssh_ps_encoded_command round-trip fixture (703) plus an embedded-quote
// case (703's own imag sibling test also exercises the double-quote-escape rule) — proves the
// args-tail quoting survives BOTH a space-bearing Windows path and an embedded `"`.
const ARGS: &[&str] = &[
    "--strih",
    r"D:\_REC\2026-07-10 17-10-31.mkv",
    "--tag",
    r#"he said "hi""#,
];
// Captured from the UNMODIFIED (pre-issue-1260) `build_onbox_command` for the exact EXE/ARGS
// above — `bash -c '. scripts/recording-verdict-on-strih.sh; build_onbox_command "C:\camera-box\
// recording-verdict.exe" --strih "D:\_REC\2026-07-10 17-10-31.mkv" --tag "he said \"hi\""'`
// printed:
//   $env:RUST_LOG="info"; & "C:\camera-box\recording-verdict.exe" "--strih" "D:\_REC\2026-07-10
//   17-10-31.mkv" "--tag" "he said ""hi"""
// This is the exact byte sequence from `& "` onward (the call operator, every double-quoted arg,
// and the doubled-`""` embedded-quote escape) that MUST survive issue 1260 unchanged — only the
// text BEFORE it (a new PriorityClass statement) may change.
const EXPECTED_ARGS_TAIL: &str = "& \"C:\\camera-box\\recording-verdict.exe\" \"--strih\" \"D:\\_REC\\2026-07-10 17-10-31.mkv\" \"--tag\" \"he said \"\"hi\"\"\"\n";

#[test]
fn default_priority_is_belownormal_before_the_call_operator_for_strih_and_stream() {
    for (label, script) in [("strih", strih_script()), ("stream", stream_script())] {
        let (stdout, stderr, ok) = run_build_onbox_command(&script, EXE, ARGS, None);
        assert!(
            ok,
            "issue 1260: {label} build_onbox_command failed: {stderr}"
        );
        let prio_pos = stdout
            .find("PriorityClass = \"BelowNormal\"")
            .unwrap_or_else(|| {
                panic!(
                    "issue 1260: {label} default (no E2E_ONBOX_DECODE_PRIORITY) must set \
                 PriorityClass = \"BelowNormal\" on the PowerShell host process (owner rule: a \
                 needed property is default-on, never a forgettable toggle). Got: {stdout:?}"
                )
            });
        let call_pos = stdout.find("& \"").unwrap_or_else(|| {
            panic!("issue 1260: {label} must still emit the `& \"<exe>\"` call. Got: {stdout:?}")
        });
        assert!(
            prio_pos < call_pos,
            "issue 1260: {label} the PriorityClass statement must come BEFORE the `& \"` call \
             operator (it sets the HOST process's priority before the child inherits it). \
             prio_pos={prio_pos} call_pos={call_pos}. Got: {stdout:?}"
        );
        assert!(
            stdout.contains("$env:RUST_LOG=\"info\";"),
            "issue 1260: {label} must still set RUST_LOG=info (unchanged). Got: {stdout:?}"
        );
    }
}

#[test]
fn env_override_normal_is_honored_for_strih_and_stream() {
    for (label, script) in [("strih", strih_script()), ("stream", stream_script())] {
        let (stdout, stderr, ok) = run_build_onbox_command(&script, EXE, ARGS, Some("Normal"));
        assert!(
            ok,
            "issue 1260: {label} build_onbox_command failed: {stderr}"
        );
        assert!(
            stdout.contains("PriorityClass = \"Normal\""),
            "issue 1260: {label} E2E_ONBOX_DECODE_PRIORITY=Normal must be honored verbatim \
             (the supervisor's A/B lever). Got: {stdout:?}"
        );
        assert!(
            !stdout.contains("BelowNormal"),
            "issue 1260: {label} an explicit Normal override must not also emit BelowNormal. \
             Got: {stdout:?}"
        );
    }
}

#[test]
fn env_override_idle_is_honored_for_strih_and_stream() {
    for (label, script) in [("strih", strih_script()), ("stream", stream_script())] {
        let (stdout, stderr, ok) = run_build_onbox_command(&script, EXE, ARGS, Some("Idle"));
        assert!(
            ok,
            "issue 1260: {label} build_onbox_command failed: {stderr}"
        );
        assert!(
            stdout.contains("PriorityClass = \"Idle\""),
            "issue 1260: {label} E2E_ONBOX_DECODE_PRIORITY=Idle must be honored. Got: {stdout:?}"
        );
    }
}

#[test]
fn invalid_env_value_falls_back_to_belownormal_with_a_stderr_warning_for_strih_and_stream() {
    for (label, script) in [("strih", strih_script()), ("stream", stream_script())] {
        let (stdout, stderr, ok) = run_build_onbox_command(&script, EXE, ARGS, Some("SuperFast"));
        assert!(
            ok,
            "issue 1260: {label} build_onbox_command failed: {stderr}"
        );
        assert!(
            stdout.contains("PriorityClass = \"BelowNormal\""),
            "issue 1260: {label} an invalid E2E_ONBOX_DECODE_PRIORITY value must fall back to \
             BelowNormal (fail-safe default), never propagate the invalid string into the \
             PowerShell text (PowerShell would reject an unrecognized PriorityClass at parse \
             time on the box). Got: {stdout:?}"
        );
        assert!(
            !stdout.contains("SuperFast"),
            "issue 1260: {label} the rejected value must never reach the emitted PowerShell \
             text. Got: {stdout:?}"
        );
        assert!(
            stderr.contains("WARNING") && stderr.contains("SuperFast"),
            "issue 1260: {label} an invalid E2E_ONBOX_DECODE_PRIORITY value must print a \
             WARNING on stderr naming the rejected value (never silently ignored — a typo'd \
             override must be visible in the run log). Got stderr: {stderr:?}"
        );
    }
}

/// The args tail — everything from the `& "<exe>"` call operator onward, including the
/// space-bearing Windows path and the doubled-`""` embedded-quote escape — must be BYTE-IDENTICAL
/// to what the pre-issue-1260 `build_onbox_command` produced for the same inputs. Only the text
/// BEFORE `& "` (RUST_LOG + the new PriorityClass statement) may change.
#[test]
fn args_tail_after_the_call_operator_is_byte_identical_to_the_pre_change_form() {
    for (label, script) in [("strih", strih_script()), ("stream", stream_script())] {
        let (stdout, stderr, ok) = run_build_onbox_command(&script, EXE, ARGS, None);
        assert!(
            ok,
            "issue 1260: {label} build_onbox_command failed: {stderr}"
        );
        let call_pos = stdout.find("& \"").unwrap_or_else(|| {
            panic!("issue 1260: {label} must emit the `& \"<exe>\"` call. Got: {stdout:?}")
        });
        let actual_tail = &stdout[call_pos..];
        assert_eq!(
            actual_tail, EXPECTED_ARGS_TAIL,
            "issue 1260: {label} the args tail (call operator + every quoted arg + the \
             embedded-quote escape) must stay byte-identical to the pre-change form — only the \
             PriorityClass prefix may change. Got tail: {actual_tail:?}"
        );
    }
}
