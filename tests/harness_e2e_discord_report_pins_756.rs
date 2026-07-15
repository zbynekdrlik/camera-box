//! #756 Member 3 — `scripts/lib/e2e-discord-report.sh` must forward an optional 5th
//! `pins-json-path` argument to `e2e_discord_report.py` as `--pins-json <path>`, ONLY when that
//! path is both non-empty and points to a file that actually exists and is non-empty (a failed
//! `scripts/latency_pins_snapshot.py` run — see `scripts/recording-e2e.sh`'s call site — passes
//! an empty string; the report composer must then omit the pins section entirely, never crash
//! on a missing/bogus file).
//!
//! Drives the REAL `e2e_discord_report_send` function (sourced, not re-implemented) against a
//! fake `python3` on PATH that records its argv — proving the actual CLI invocation, not a
//! re-spelling of the script's intent. Mirrors `tests/harness_e2e_discord_report_owner_thread_
//! 719.rs`'s own fake-binary technique (that file fakes `curl`; this one fakes `python3` since
//! the behavior under test is upstream of the curl POST).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    manifest_dir().join("scripts/lib/e2e-discord-report.sh")
}

fn set_exec(p: &Path) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

struct RunOutcome {
    exit_code: i32,
    stderr: String,
    python_argv: Vec<String>,
}

/// Run `e2e_discord_report_send <verdict> test-run-id 0 300 <pins_arg>` with fake `python3` +
/// `curl` binaries on PATH. `pins_arg` is passed through unquoted-arg-boundary-safe via a
/// separate positional so an empty string is a real empty 5th argument (not omitted).
fn run_send_with_pins(pins_arg: &str, pins_file: Option<(&str, &str)>) -> RunOutcome {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let home_dir = tmp.path().join("home");
    fs::create_dir_all(&home_dir).unwrap();
    let python_log = tmp.path().join("python.log");

    // Fake python3: records argv (one per line — none of these args legitimately contain a
    // literal newline), then prints a valid JSON chunks array so the sender's `jq 'length'`
    // sees a real, non-empty result and proceeds to "POST" via fake curl.
    let fake_python = bin_dir.join("python3");
    fs::write(
        &fake_python,
        "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> \"$PYTHON_LOG\"\nprintf '[\"stub report\"]'\n",
    )
    .unwrap();
    set_exec(&fake_python);

    let fake_curl = bin_dir.join("curl");
    fs::write(
        &fake_curl,
        "#!/usr/bin/env bash\nprintf '{\"id\":\"1\"}\\n200'\n",
    )
    .unwrap();
    set_exec(&fake_curl);

    let verdict_json = tmp.path().join("verdict.json");
    fs::write(&verdict_json, "{}").unwrap();

    let pins_path_str: String = match pins_file {
        Some((name, contents)) => {
            let p = tmp.path().join(name);
            fs::write(&p, contents).unwrap();
            p.display().to_string()
        }
        None => pins_arg.to_string(),
    };

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(format!(
        "set -uo pipefail; . {:?}; e2e_discord_report_send {:?} test-run-id 0 300 {:?}",
        script_path(),
        verdict_json,
        pins_path_str,
    ));
    cmd.env_remove("DISCORD_BOT_TOKEN")
        .env_remove("DISCORD_CHANNEL_ID")
        .env_remove("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK")
        .env_remove("DISCORD_MENTION_ZBYNEK")
        .env_remove("GITHUB_ACTIONS")
        .env("PATH", &path_env)
        .env("HOME", &home_dir)
        .env("DISCORD_BOT_TOKEN", "test-bot-token")
        .env("PYTHON_LOG", &python_log);

    let out = cmd.output().expect("run e2e_discord_report_send");
    let python_argv = if python_log.exists() {
        fs::read_to_string(&python_log)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    };
    RunOutcome {
        exit_code: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        python_argv,
    }
}

#[test]
fn pins_json_path_present_and_non_empty_forwards_pins_json_flag() {
    let out = run_send_with_pins("", Some(("pins.json", "{\"strih\":{}}")));
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
    assert!(
        out.python_argv.iter().any(|a| a == "--pins-json"),
        "a non-empty, existing pins file must be forwarded as --pins-json; argv={:?}",
        out.python_argv
    );
    let idx = out
        .python_argv
        .iter()
        .position(|a| a == "--pins-json")
        .unwrap();
    assert!(
        out.python_argv[idx + 1].ends_with("pins.json"),
        "the --pins-json value must be the actual snapshot file path, got {:?}",
        out.python_argv.get(idx + 1)
    );
}

#[test]
fn empty_pins_arg_omits_pins_json_flag() {
    // Mirrors recording-e2e.sh's own fail-open path: latency_pins_snapshot.py failed, so
    // PINS_JSON is reset to "" before e2e_discord_report_send is called.
    let out = run_send_with_pins("", None);
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
    assert!(
        !out.python_argv.iter().any(|a| a == "--pins-json"),
        "an empty pins-json-path arg must NOT forward --pins-json (never point at a bogus/absent \
         file); argv={:?}",
        out.python_argv
    );
}

#[test]
fn nonexistent_pins_path_omits_pins_json_flag() {
    // A stale/deleted path (never written, or cleaned up between steps) must be treated the
    // same as "no pins" -- never pass a --pins-json the composer would fail to open.
    let out = run_send_with_pins("/tmp/this-path-does-not-exist-756.json", None);
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
    assert!(
        !out.python_argv.iter().any(|a| a == "--pins-json"),
        "a nonexistent pins path must NOT forward --pins-json; argv={:?}",
        out.python_argv
    );
}
