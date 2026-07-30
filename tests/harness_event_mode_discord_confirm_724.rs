//! #724 — EVENT-mode Discord confirmation: after the #722 contract assert phase, ONE Slovak
//! message lands in the owner's Discord thread with @mention, on BOTH outcomes (pass = a
//! confirmation, fail = a warning naming the failing items) — so the user never again has to
//! trust a bare terminal claim about the rig being broadcast-clean (the 2026-07-12 incident,
//! #721: the user caught a live QR by EYE; a phone confirmation would have surfaced it, or its
//! ABSENCE would itself have been the alarm).
//!
//! Delivery reuses the #719 owner-thread + @mention model
//! (DISCORD_NOTIFICATION_CHANNEL_ZBYNEK and DISCORD_MENTION_ZBYNEK, sourced from
//! ~/.claude/channels/discord/.env when not already in the environment) — but UNLIKE #719's
//! e2e-discord-report.sh, this NEVER falls back to #notifications: per the ticket's own
//! instruction ("share the sender; do NOT post to #notifications"), when the owner vars are
//! genuinely absent this sender logs loudly and skips sending entirely (fail-open — a missing
//! confirmation must never fail rig-mode.sh event).
//!
//! These tests drive the REAL `event_mode_discord_confirm_send` function (sourced, not
//! re-implemented) against a fake `curl` on PATH that records every invocation's argv.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    manifest_dir().join("scripts/lib/event-mode-discord-confirm.sh")
}

fn set_exec(p: &Path) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

type CurlCall = Vec<String>;

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
    stderr: String,
    curl_calls: Vec<CurlCall>,
}

fn run_send(message: &str, home_files: &[(&str, &str)], extra_env: &[(&str, &str)]) -> SendOutcome {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let home_dir = tmp.path().join("home");
    fs::create_dir_all(&home_dir).unwrap();
    let curl_log = tmp.path().join("curl.log");

    let fake_curl = bin_dir.join("curl");
    fs::write(
        &fake_curl,
        "#!/usr/bin/env bash\n\
         {\n  for a in \"$@\"; do printf '%s\\x1e' \"$a\"; done\n  printf '\\x1d'\n} >> \"$CURL_LOG\"\n\
         printf '{\"id\":\"111222333\"}\\n200'\n",
    )
    .unwrap();
    set_exec(&fake_curl);

    for (rel, contents) in home_files {
        let p = home_dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, contents).unwrap();
    }

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(format!(
        "set -uo pipefail; . {:?}; event_mode_discord_confirm_send {:?}",
        script_path(),
        message,
    ));
    cmd.env_remove("DISCORD_BOT_TOKEN")
        .env_remove("DISCORD_CHANNEL_ID")
        .env_remove("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK")
        .env_remove("DISCORD_MENTION_ZBYNEK")
        .env("PATH", &path_env)
        .env("HOME", &home_dir)
        .env("CURL_LOG", &curl_log);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let out = cmd.output().expect("run event_mode_discord_confirm_send");
    SendOutcome {
        exit_code: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        curl_calls: parse_curl_log(&curl_log),
    }
}

fn find_arg_after<'a>(call: &'a [String], flag: &str) -> Option<&'a str> {
    let idx = call.iter().position(|a| a == flag)?;
    call.get(idx + 1).map(|s| s.as_str())
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

#[test]
fn owner_channel_and_mention_present_routes_to_owner_thread_with_mention() {
    let out = run_send(
        "EVENT mod POTVRDENY -- test message",
        &[],
        &[
            ("DISCORD_BOT_TOKEN", "test-bot-token"),
            ("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK", "555000111"),
            ("DISCORD_MENTION_ZBYNEK", "777222333"),
        ],
    );
    assert_eq!(
        out.exit_code, 0,
        "event_mode_discord_confirm_send must always return 0 (fail-open), stderr={}",
        out.stderr
    );
    assert_eq!(out.curl_calls.len(), 1, "stderr={}", out.stderr);
    let call = &out.curl_calls[0];
    assert!(
        url(call).contains("/channels/555000111/messages"),
        "must POST to the owner's thread channel id, got url={}",
        url(call)
    );
    let content = payload_content(call);
    assert!(
        content.starts_with("<@777222333> "),
        "must be prefixed with the owner's @mention (the push trigger), got: {content:?}"
    );
    assert!(content.contains("EVENT mod POTVRDENY -- test message"));
}

#[test]
fn owner_vars_absent_skips_sending_never_falls_back_to_notifications() {
    let out = run_send(
        "EVENT mod NEPRESIEL -- test message",
        &[],
        &[("DISCORD_BOT_TOKEN", "test-bot-token")],
    );
    assert_eq!(
        out.exit_code, 0,
        "must stay fail-open, stderr={}",
        out.stderr
    );
    assert_eq!(
        out.curl_calls.len(),
        0,
        "must NEVER post to #notifications when the owner vars are absent (per the ticket's own \
         'do not post to #notifications' instruction) -- got calls: {:?}",
        out.curl_calls
    );
    assert!(
        out.stderr.contains("DISCORD_NOTIFICATION_CHANNEL_ZBYNEK")
            || out.stderr.to_lowercase().contains("skip"),
        "must log loudly that the confirmation was skipped, stderr={}",
        out.stderr
    );
}

#[test]
fn sourced_env_backfills_owner_vars_without_clobbering_a_preset_bot_token() {
    let dotenv = "DISCORD_BOT_TOKEN=local-env-token-should-NOT-be-used\n\
                  DISCORD_NOTIFICATION_CHANNEL_ZBYNEK=555000111\n\
                  DISCORD_MENTION_ZBYNEK=777222333\n";
    let out = run_send(
        "EVENT mod POTVRDENY",
        &[(".claude/channels/discord/.env", dotenv)],
        &[("DISCORD_BOT_TOKEN", "ci-secret-token")],
    );
    assert_eq!(out.curl_calls.len(), 1, "stderr={}", out.stderr);
    let call = &out.curl_calls[0];
    let auth = call
        .iter()
        .find(|a| a.starts_with("Authorization: Bot "))
        .expect("curl call must carry an Authorization header");
    assert_eq!(
        auth, "Authorization: Bot ci-secret-token",
        "a preset DISCORD_BOT_TOKEN must never be overwritten by the sourced local .env's own \
         token value"
    );
    assert!(url(call).contains("/channels/555000111/messages"));
}

#[test]
fn missing_bot_token_entirely_fails_open_no_crash() {
    let out = run_send("EVENT mod POTVRDENY", &[], &[]);
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
    assert_eq!(out.curl_calls.len(), 0);
}

// ---------------------------------------------------------------------------
// Static wiring: rig-mode.sh's do_event() must call the #724 sender on BOTH outcomes -- never
// only inside the pass branch or only inside the fail branch.
// ---------------------------------------------------------------------------

#[test]
fn rig_mode_sources_the_724_sender() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap();
    assert!(
        text.contains("lib/event-mode-discord-confirm.sh"),
        "rig-mode.sh must source scripts/lib/event-mode-discord-confirm.sh"
    );
}

#[test]
fn do_event_calls_the_724_sender_on_both_outcomes() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap();
    let start = text.find("do_event() {").expect("do_event() must exist");
    let end = text[start..]
        .find("\nmain() {")
        .map(|off| start + off)
        .unwrap_or(text.len());
    let body = &text[start..end];
    // The call site must appear UNCONDITIONALLY after event_mode_assert (not nested inside an
    // `if [ "$EVENT_ASSERT_PASS" -eq 0 ]` block, which would only fire on pass).
    let assert_pos = body
        .find("event_mode_assert")
        .expect("do_event() must call event_mode_assert");
    let sender_pos = body
        .find("event_mode_discord_confirm_send")
        .expect("do_event() must call event_mode_discord_confirm_send");
    assert!(
        sender_pos > assert_pos,
        "the Discord confirmation must be sent AFTER the assert phase runs (needs its verdict)"
    );
    // It must not sit inside an if-branch keyed on EVENT_ASSERT_PASS==0 specifically -- find the
    // narrowest such conditional block and confirm the sender call is NOT inside it.
    if let Some(if_pos) = body.find("if [ \"$EVENT_ASSERT_PASS\" -eq 0 ]") {
        if let Some(fi_pos) = body[if_pos..].find("\n  fi") {
            let branch = &body[if_pos..if_pos + fi_pos];
            assert!(
                !branch.contains("event_mode_discord_confirm_send"),
                "the Discord confirmation must fire on BOTH outcomes, not only inside the PASS \
                 branch"
            );
        }
    }
}
