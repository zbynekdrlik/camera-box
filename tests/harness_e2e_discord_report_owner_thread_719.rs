//! #719 — `scripts/lib/e2e-discord-report.sh` must route the #711 full-report Discord post to
//! the OWNER's own thread WITH their @mention (the push-notification trigger), not to the
//! shared #notifications channel with no mention.
//!
//! Root cause (issue #719, user-confirmed 2026-07-12): the sender always posted to
//! `DISCORD_CHANNEL_ID` (default `1257652233714270219`, the #notifications channel) with a bare
//! `{content: "..."}` payload — no `<@id>` mention anywhere. The user does not watch
//! #notifications, and Discord only pushes a phone notification on an @mention (or a DM) — so
//! every #711 report since it shipped was silently invisible to its only intended reader.
//!
//! Fix direction (per the issue body + `milestone-notifications.md`'s owner-thread + @mention
//! model): prefer `DISCORD_NOTIFICATION_CHANNEL_ZBYNEK` (the owner's own Discord thread) and
//! prefix the FIRST message chunk with `<@$DISCORD_MENTION_ZBYNEK> `. Both vars live in
//! `~/.claude/channels/discord/.env`, the SAME file the sender already sources for
//! `DISCORD_BOT_TOKEN` when that isn't already in the environment — but CI (`full-path-e2e.yml`)
//! passes only `DISCORD_BOT_TOKEN`/`DISCORD_CHANNEL_ID` as GitHub secrets, never the owner vars,
//! so the sender must source the .env for the owner vars EVEN when `DISCORD_BOT_TOKEN` is
//! already set (self-hosted runner on dev1 — the .env file is right there) — while never letting
//! that sourcing clobber an already-set `DISCORD_BOT_TOKEN` (CI's real secret must always win
//! over whatever token value happens to sit in the local .env).
//!
//! #notifications stays as a LOGGED FALLBACK only when the owner vars are genuinely absent.
//!
//! These tests drive the REAL `e2e_discord_report_send` function (sourced, not re-implemented)
//! against a fake `curl` on PATH that records every invocation's argv — proving the actual
//! channel URL and payload content, not a re-spelling of the script's intent.

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

/// One recorded fake-`curl` invocation: its argv, in order.
type CurlCall = Vec<String>;

/// Args can (and do — the composed Slovak report is multi-line) contain embedded newlines, so a
/// newline-delimited log is ambiguous. The fake curl instead separates ARGS with ASCII Record
/// Separator (`\x1e`) and CALLS with ASCII Group Separator (`\x1d`) — bytes that never occur in
/// real curl argv (headers, JSON payload, URL).
fn parse_curl_log(log: &Path) -> Vec<CurlCall> {
    if !log.exists() {
        return vec![];
    }
    let raw = fs::read_to_string(log).unwrap();
    raw.split('\u{1d}')
        .filter(|block| !block.is_empty())
        .map(|block| {
            block
                .split('\u{1e}')
                .filter(|a| !a.is_empty())
                .map(|a| a.to_string())
                .collect()
        })
        .collect()
}

struct SendOutcome {
    exit_code: i32,
    stdout: String,
    stderr: String,
    curl_calls: Vec<CurlCall>,
}

/// Drive the real `e2e_discord_report_send` with a fake `curl` on PATH.
///
/// `home_env` — files to pre-create under a fresh HOME (e.g. a `.claude/channels/discord/.env`)
/// before the sourced function runs; pass `&[]` for an empty HOME (no local .env at all).
/// `extra_env` — env vars set directly on the child process (simulating CI secrets / operator env).
fn run_send(home_files: &[(&str, &str)], extra_env: &[(&str, &str)]) -> SendOutcome {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let home_dir = tmp.path().join("home");
    fs::create_dir_all(&home_dir).unwrap();
    let curl_log = tmp.path().join("curl.log");

    // Fake curl: records argv to CURL_LOG using \x1e between args and \x1d between calls (see
    // parse_curl_log — a real arg, e.g. the multi-line composed report payload, can itself
    // contain newlines, so a newline-delimited log would be ambiguous), then prints a canned
    // Discord success response ({"id":...}\n200), mirroring `curl -sS -w '\n%{http_code}' ...`.
    let fake_curl = bin_dir.join("curl");
    fs::write(
        &fake_curl,
        "#!/usr/bin/env bash\n\
         {\n  for a in \"$@\"; do printf '%s\\x1e' \"$a\"; done\n  printf '\\x1d'\n} >> \"$CURL_LOG\"\n\
         printf '{\"id\":\"999888777\"}\\n200'\n",
    )
    .unwrap();
    set_exec(&fake_curl);

    for (rel, contents) in home_files {
        let p = home_dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, contents).unwrap();
    }

    let verdict_json = tmp.path().join("verdict.json");
    fs::write(&verdict_json, "{}").unwrap();

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(format!(
        "set -uo pipefail; . {:?}; e2e_discord_report_send {:?} test-run-id 0 300",
        script_path(),
        verdict_json,
    ));
    cmd.env_remove("DISCORD_BOT_TOKEN")
        .env_remove("DISCORD_CHANNEL_ID")
        .env_remove("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK")
        .env_remove("DISCORD_MENTION_ZBYNEK")
        .env_remove("GITHUB_ACTIONS")
        .env("PATH", &path_env)
        .env("HOME", &home_dir)
        .env("CURL_LOG", &curl_log);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let out = cmd.output().expect("run e2e_discord_report_send");
    SendOutcome {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        curl_calls: parse_curl_log(&curl_log),
    }
}

fn find_arg_after<'a>(call: &'a [String], flag: &str) -> Option<&'a str> {
    let idx = call.iter().position(|a| a == flag)?;
    call.get(idx + 1).map(|s| s.as_str())
}

fn find_header<'a>(call: &'a [String], prefix: &str) -> Option<&'a str> {
    call.iter()
        .find(|a| a.starts_with(prefix))
        .map(|s| s.as_str())
}

fn payload_content(call: &[String]) -> String {
    let payload = find_arg_after(call, "-d").expect("curl call missing -d payload");
    let v: serde_json::Value = serde_json::from_str(payload).expect("payload is valid JSON");
    v.get("content")
        .and_then(|c| c.as_str())
        .expect("payload has a string .content")
        .to_string()
}

fn url(call: &[String]) -> &str {
    call.last().expect("curl call has a URL as its last arg")
}

// ---------------------------------------------------------------------------
// RED: current (pre-fix) behavior — owner vars provided, but the report STILL goes to the
// hard-coded #notifications default with no mention, because the sender never looks at
// DISCORD_NOTIFICATION_CHANNEL_ZBYNEK / DISCORD_MENTION_ZBYNEK at all.
// ---------------------------------------------------------------------------

#[test]
fn owner_channel_and_mention_present_routes_to_owner_thread_with_mention() {
    let out = run_send(
        &[],
        &[
            ("DISCORD_BOT_TOKEN", "test-bot-token"),
            ("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK", "555000111"),
            ("DISCORD_MENTION_ZBYNEK", "777222333"),
        ],
    );
    assert_eq!(
        out.exit_code, 0,
        "e2e_discord_report_send must always return 0 (fail-open). stdout={} stderr={}",
        out.stdout, out.stderr
    );
    assert_eq!(
        out.curl_calls.len(),
        1,
        "expected exactly one curl POST for a short (empty-verdict) report; stderr={}",
        out.stderr
    );
    let call = &out.curl_calls[0];
    assert!(
        url(call).contains("/channels/555000111/messages"),
        "must POST to the owner's thread channel id (DISCORD_NOTIFICATION_CHANNEL_ZBYNEK), got url={}",
        url(call)
    );
    let content = payload_content(call);
    assert!(
        content.starts_with("<@777222333> "),
        "chunk 1 must be prefixed with the owner's @mention (the Discord push trigger), got: {content:?}"
    );
}

#[test]
fn owner_channel_present_no_mention_var_warns_but_still_routes_to_owner_thread() {
    let out = run_send(
        &[],
        &[
            ("DISCORD_BOT_TOKEN", "test-bot-token"),
            ("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK", "555000111"),
            // DISCORD_MENTION_ZBYNEK intentionally absent
        ],
    );
    assert_eq!(out.curl_calls.len(), 1, "stderr={}", out.stderr);
    let call = &out.curl_calls[0];
    assert!(
        url(call).contains("/channels/555000111/messages"),
        "still must route to the owner thread even without a mention var, got url={}",
        url(call)
    );
    assert!(
        out.stderr.contains("DISCORD_MENTION_ZBYNEK"),
        "must log a warning that no mention is available (silent no-push is a regression), stderr={}",
        out.stderr
    );
}

#[test]
fn owner_vars_absent_falls_back_to_notifications_channel_logged() {
    let out = run_send(
        &[],
        &[
            ("DISCORD_BOT_TOKEN", "test-bot-token"),
            // no owner vars, no local .env present (empty HOME) -> genuinely absent
        ],
    );
    assert_eq!(out.curl_calls.len(), 1, "stderr={}", out.stderr);
    let call = &out.curl_calls[0];
    assert!(
        url(call).contains("/channels/1257652233714270219/messages"),
        "with owner vars genuinely absent, must fall back to the #notifications default, got url={}",
        url(call)
    );
    let content = payload_content(call);
    assert!(
        !content.starts_with("<@"),
        "the #notifications fallback must NOT carry an owner mention, got: {content:?}"
    );
    assert!(
        out.stderr.contains("falling back to #notifications")
            || out
                .stderr
                .contains("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK not set"),
        "the fallback must be LOGGED loudly (per the issue's own requirement), stderr={}",
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// CI-secret precedence: a self-hosted-runner .env backfills the OWNER vars (never passed as CI
// secrets) but must NEVER clobber an already-set DISCORD_BOT_TOKEN (the real GitHub secret).
// ---------------------------------------------------------------------------

#[test]
fn sourced_env_backfills_owner_vars_without_clobbering_a_preset_bot_token() {
    let dotenv = "DISCORD_BOT_TOKEN=local-env-token-should-NOT-be-used\n\
                  DISCORD_NOTIFICATION_CHANNEL_ZBYNEK=555000111\n\
                  DISCORD_MENTION_ZBYNEK=777222333\n";
    let out = run_send(
        &[(".claude/channels/discord/.env", dotenv)],
        &[
            // Simulates CI's real GitHub secret — must win over the .env's own token value.
            ("DISCORD_BOT_TOKEN", "ci-secret-token"),
        ],
    );
    assert_eq!(out.curl_calls.len(), 1, "stderr={}", out.stderr);
    let call = &out.curl_calls[0];
    let auth = find_header(call, "Authorization: Bot ")
        .expect("curl call must carry an Authorization header");
    assert_eq!(
        auth, "Authorization: Bot ci-secret-token",
        "a preset DISCORD_BOT_TOKEN (CI's real secret) must never be overwritten by the sourced \
         local .env's own token value"
    );
    assert!(
        url(call).contains("/channels/555000111/messages"),
        "the owner channel id must still be backfilled from the sourced .env even though \
         DISCORD_BOT_TOKEN was already set, got url={}",
        url(call)
    );
    let content = payload_content(call);
    assert!(
        content.starts_with("<@777222333> "),
        "the mention must also be backfilled from the sourced .env, got: {content:?}"
    );
}
