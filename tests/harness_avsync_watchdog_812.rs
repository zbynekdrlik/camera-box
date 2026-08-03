//! #812 — avsync watchdog scheduled-task / Discord-webhook / heartbeat fix.
//!
//! Background: `C:\avsync\watchdog.ps1` (previously hand-built only on the stream box, never
//! tracked in this repo) has THREE live defects: (1) its scheduled task is ONSTART-only, never
//! retried on a crash/hang between boots; (2) it never passes `--webhook` to
//! `av_sync_measure.py`, so even a perfectly alive watchdog posts nothing to Discord; (3) it
//! writes no heartbeat, so silence and health are indistinguishable from outside the box. This
//! file proves `scripts/avsync-watchdog.ps1` (the repo-tracked replacement) fixes (2) and (3) and
//! keeps the pre-existing #814 grab-freshness gate untouched, `scripts/avsync-keepalive.ps1`
//! fixes (1) via a periodic check-and-relaunch, and `scripts/avsync-watchdog-install.sh` (the
//! pure planner) emits the correct Task Scheduler XML for both the new keep-alive task and the
//! new vlc-monitor task (#807's own file gets its own dedicated test file).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const WATCHDOG_PS1: &str = "scripts/avsync-watchdog.ps1";
const KEEPALIVE_PS1: &str = "scripts/avsync-keepalive.ps1";
const INSTALL_SH: &str = "scripts/avsync-watchdog-install.sh";

// ================================================================================================
// scripts/avsync-watchdog.ps1 — defects 2 (webhook) and 3 (heartbeat) + the bounded-measurement
// hardening (the "two orphan python PIDs" root-cause evidence).
// ================================================================================================

#[test]
fn avsync_watchdog_ps1_wires_the_discord_webhook_from_an_uncommitted_local_file() {
    let body = read(WATCHDOG_PS1);
    assert!(
        body.contains("discord-webhook.txt"),
        "the webhook URL must be read from an UNCOMMITTED local file on the box, never a repo literal (security-basics.md)"
    );
    assert!(
        body.contains("Get-Webhook"),
        "must have a function reading the local webhook config"
    );
    assert!(
        body.contains("'--webhook'") || body.contains("\"--webhook\""),
        "must actually pass --webhook through to av_sync_measure.py when configured"
    );
    // Never a literal webhook URL (discord.com/api/webhooks/...) committed to the repo.
    assert!(
        !body.contains("discord.com/api/webhooks/"),
        "must NEVER hardcode a real Discord webhook URL in the repo"
    );
}

#[test]
fn avsync_watchdog_ps1_writes_a_heartbeat_every_pass_regardless_of_outcome() {
    let body = read(WATCHDOG_PS1);
    assert!(
        body.contains("Write-Heartbeat"),
        "must write a heartbeat signal so silence and health are no longer indistinguishable"
    );
    assert!(
        body.contains("avsync-watchdog-heartbeat.txt"),
        "heartbeat must land at the well-known path scripts/avsync-heartbeat-alert-watchdog.sh polls"
    );
    // Heartbeat must be written on BOTH the no-signal branch and the measured branch -- not just
    // the happy path (a heartbeat that only fires on success is not a liveness signal).
    // Anchor on the FULL log-line text, not a bare "NO-SIGNAL" substring -- the header comment
    // above also mentions NO-SIGNAL in prose, and `.find()` would otherwise grab that occurrence
    // instead of the real call site (see .claude/rules/ci-testing-gotchas.md's anchor-collision
    // class of bug -- self-collision within a single new file/test, not just against another).
    let no_signal_idx = body
        .find("LIVE :: NO-SIGNAL - no verdict")
        .expect("must keep the existing NO-SIGNAL log line");
    let measured_idx = body
        .find("LIVE :: $out")
        .expect("must keep the existing measured-result log line");
    let no_signal_region = &body[no_signal_idx..(no_signal_idx + 200).min(body.len())];
    let measured_region = &body[measured_idx..(measured_idx + 200).min(body.len())];
    assert!(
        no_signal_region.contains("Write-Heartbeat"),
        "the no-signal branch must ALSO write a heartbeat -- an idle stream must not look dead"
    );
    assert!(
        measured_region.contains("Write-Heartbeat"),
        "the measured branch must write a heartbeat too"
    );
}

#[test]
fn avsync_watchdog_ps1_bounds_the_measurement_call_never_wedges_on_a_hung_python() {
    let body = read(WATCHDOG_PS1);
    assert!(
        body.contains("Invoke-Measurement"),
        "the python measurement call must be bounded -- the root incident's own evidence was \
         'two orphan python PIDs...consistent with a hung av_sync_measure.py that took the loop down with it'"
    );
    assert!(
        body.contains("180000"),
        "must have an explicit millisecond timeout on the measurement call"
    );
    assert!(
        body.contains("Stop-Process -Id $proc.Id -Force"),
        "a timed-out measurement must be force-killed by its OWN process handle (not a job-in-a-\
         child-shell, which can leave the real python.exe orphaned exactly like the live incident)"
    );
}

#[test]
fn avsync_watchdog_ps1_keeps_the_814_grab_freshness_gate_unchanged() {
    let body = read(WATCHDOG_PS1);
    // These are the exact #814 grab-freshness thresholds confirmed live on the box -- this fix
    // must NOT touch them (design decision: "same grab-freshness gate untouched").
    assert!(
        body.contains("200000"),
        "clip-too-small threshold (bytes) must be unchanged"
    );
    assert!(
        body.contains("180"),
        "clip-stale age threshold (seconds) must be unchanged"
    );
    assert!(
        body.contains("< 20"),
        "clip-too-short duration threshold (seconds) must be unchanged"
    );
    assert!(
        body.contains("rtmp://127.0.0.1:1234/live/obs-e2e-test"),
        "the grab URL is confirmed CORRECT (it IS the real production broadcast per service.json) \
         -- must be unchanged, never 'fixed' away"
    );
}

// ================================================================================================
// scripts/avsync-keepalive.ps1 — defect 1 (ONSTART-only task, no periodic keep-alive).
// ================================================================================================

#[test]
fn avsync_keepalive_ps1_checks_and_relaunches_both_long_running_loops() {
    let body = read(KEEPALIVE_PS1);
    assert!(
        body.contains("Ensure-Running"),
        "must have a reusable check-and-relaunch function (one mechanism, not two copy-pasted ones)"
    );
    assert!(
        body.contains("'watchdog.ps1'"),
        "must keep-alive the #812 avsync measurement loop"
    );
    assert!(
        body.contains("'avsync-vlc-monitor.ps1'"),
        "must ALSO keep-alive the #807 VLC monitor -- one shared keep-alive task for both loops"
    );
    assert!(
        body.contains("Win32_Process") && body.contains("CommandLine"),
        "must match on the process CommandLine (not just process name) to avoid confusing this \
         check with any other powershell.exe on the box"
    );
}

#[test]
fn ensure_running_matches_the_full_invocation_path_never_a_bare_script_name_968() {
    // #968: a bare script-name substring match (`-like "*$scriptName*"`) counts ANY process whose
    // command-line TEXT merely quotes that filename as "already running" -- confirmed live
    // 2026-08-03: a diagnostic MCP powershell whose command text contained the literal
    // "watchdog.ps1" inside an unrelated Get-CimInstance filter string was counted as the real
    // watchdog, masking a genuinely dead process. The match must anchor on the FULL invocation:
    // the "-File" flag immediately adjacent to the exact script PATH, not just the bare filename.
    let body = read(KEEPALIVE_PS1);
    let where_idx = body
        .find("Where-Object")
        .expect("must have a Where-Object filter");
    let filter_region = &body[where_idx..(where_idx + 220).min(body.len())];
    assert!(
        filter_region.contains("-File"),
        "the match must anchor on the -File invocation flag, never a bare filename substring \
         with no launch-flag context: {filter_region}"
    );
    assert!(
        filter_region.contains("$scriptPath") || filter_region.contains("$escapedPath"),
        "the match must require the FULL script PATH parameter, not just $scriptName: {filter_region}"
    );
    assert!(
        !filter_region.contains("*$scriptName*"),
        "must NOT match on a bare $scriptName substring -- any process merely quoting the \
         filename text (e.g. a diagnostic shell's own filter string) would falsely count as \
         'already running': {filter_region}"
    );
}

// ================================================================================================
// scripts/avsync-watchdog-install.sh — PURE planner: Task Scheduler XML for the keep-alive task
// (Repetition trigger, ships disabled) and the new vlc-monitor task (BootTrigger).
// ================================================================================================

fn run_sourced(body: &str) -> (i32, String, String) {
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$SCRIPT\"\nset +e\n{body}"))
        .env("SCRIPT", script)
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn build_boot_task_xml_produces_a_boottrigger_only_no_repetition() {
    let (code, out, err) = run_sourced(
        r#"build_boot_task_xml "avsync-vlc-monitor" 'C:\avsync\avsync-vlc-monitor.ps1' "desc""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("<BootTrigger>"),
        "must be a boot trigger: {out}"
    );
    assert!(
        !out.contains("<Repetition>"),
        "the boot task must NOT have a repetition -- that's the separate keep-alive task's job"
    );
    assert!(
        out.contains("avsync-vlc-monitor.ps1"),
        "must target the vlc-monitor script: {out}"
    );
}

#[test]
fn build_boot_task_xml_names_the_task_in_its_own_description() {
    // Review-caught: unlike its mirrored sibling (obs-self-heal-install.sh's build_task_xml,
    // which threads task_name into <Description>), this function accepted task_name but never
    // referenced it in the emitted XML -- a real (if cosmetic) deviation from the pattern it
    // explicitly claims to mirror.
    let (code, out, err) = run_sourced(
        r#"build_boot_task_xml "avsync-vlc-monitor" 'C:\avsync\avsync-vlc-monitor.ps1' "desc""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    let desc_idx = out
        .find("<Description>")
        .expect("must have a Description element");
    let desc_region = &out[desc_idx..(desc_idx + 120).min(out.len())];
    assert!(
        desc_region.contains("avsync-vlc-monitor"),
        "the task name must appear in its own Description, matching obs-self-heal-install.sh's \
         own build_task_xml convention: {desc_region}"
    );
}

#[test]
fn build_keepalive_task_xml_has_a_repetition_trigger_and_ships_disabled() {
    let (code, out, err) = run_sourced(
        r#"build_keepalive_task_xml "avsync-keepalive" 'C:\avsync\avsync-keepalive.ps1' "5""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("<Repetition>") && out.contains("<Interval>PT5M</Interval>"),
        "must fire every 5 min via a Repetition trigger: {out}"
    );
    assert!(
        out.contains("<Enabled>false</Enabled>"),
        "must SHIP DISABLED -- the supervisor live-verifies before enabling (this repo's standing \
         convention for any new scheduled Windows job touching process lifecycle): {out}"
    );
    assert!(
        out.contains("avsync-keepalive.ps1"),
        "must target the keepalive script: {out}"
    );
}

#[test]
fn build_keepalive_task_xml_also_has_a_boot_trigger_matching_its_own_design_comment() {
    // Review-caught: issue 812's own design comment says the keep-alive task is invoked "every
    // ~5 min PLUS a BootTrigger" -- the shipped XML had only a Repetition TimeTrigger, no boot
    // trigger, a real (if low-impact) deviation from the stated design.
    let (code, out, err) = run_sourced(
        r#"build_keepalive_task_xml "avsync-keepalive" 'C:\avsync\avsync-keepalive.ps1' "5""#,
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("<BootTrigger>"),
        "must ALSO fire once at boot (design-comment fidelity), not rely solely on the first \
         Repetition tick: {out}"
    );
}

#[test]
fn install_plan_covers_all_three_files_and_both_new_tasks() {
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg(&script)
        .output()
        .expect("run install script directly");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "avsync-watchdog.ps1",
        "avsync-vlc-monitor.ps1",
        "avsync-keepalive.ps1",
        "discord-webhook.txt",
        "avsync-vlc-monitor",
        "avsync-keepalive",
        "/ENABLE",
    ] {
        assert!(
            plan.contains(needle),
            "install plan missing '{needle}': {plan}"
        );
    }
}

#[test]
fn install_plan_produces_empty_stderr_968() {
    // #968: the plan is printed via an UNQUOTED `cat <<PLAN` heredoc (needed for ${mcp}/${box_ip}
    // interpolation) whose body ALSO contains backtick-quoted task names -- bash command-
    // substitutes each unescaped pair, which fails ("command not found") and silently deletes the
    // backticked text from the printed plan. Confirmed live: "line 170: avsync-watchdog: command
    // not found" on stderr. A clean run must produce NO stderr output at all.
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg(&script)
        .output()
        .expect("run install script directly");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.trim().is_empty(),
        "the heredoc must never command-substitute backtick-quoted names in its own plan text: \
         stderr={stderr}"
    );
}

#[test]
fn install_plan_keeps_the_bare_task_names_literally_968() {
    // #968: the bare (non-`.ps1`-suffixed) task-name literals `avsync-watchdog` and
    // `avsync-vlc-monitor` were backtick-quoted UNESCAPED in the heredoc body (STEP 0 / STEP 2)
    // and got silently command-substituted away -- the pre-968 `install_plan_covers_all_three_
    // files_and_both_new_tasks` test never caught this because it only checked the `.ps1`-
    // suffixed file names (never backtick-quoted) and the plan's OTHER, already-escaped mentions
    // of these same task names (e.g. STEP 1's `` \`avsync-watchdog\` ``) kept the bare substring
    // present overall even with STEP 0's occurrence deleted. Anchor on the EXACT phrases around
    // the two previously-unescaped occurrences so this genuinely discriminates the bug.
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg(&script)
        .output()
        .expect("run install script directly");
    assert!(out.status.success());
    let plan = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "the current `avsync-watchdog` task already targets",
        "register the NEW `avsync-vlc-monitor` task",
    ] {
        assert!(
            plan.contains(needle),
            "backtick-quoted phrase '{needle}' must survive in the printed plan, not be \
             silently command-substituted away: {plan}"
        );
    }
}

#[test]
fn install_script_usage_and_source_guard_work() {
    let script = manifest_dir().join(INSTALL_SH);
    let out = Command::new("bash")
        .arg(&script)
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage:"));

    // Sourcing (the pure-function tests above) must never execute main() -- confirmed by the
    // fact those tests only observe the function they explicitly call, never the full plan.
    let (code, out2, err) = run_sourced("echo SOURCED_OK");
    assert_eq!(code, 0, "stderr={err}");
    assert!(out2.contains("SOURCED_OK"));
}
