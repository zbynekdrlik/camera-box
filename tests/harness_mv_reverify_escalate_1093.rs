//! #1093 — the ORDERING PROOF + RECEIVER-WEDGE ESCALATION around `preflight_mv_reverify()`.
//!
//! Two remaining items on the ticket (the budget recalibration in full-path-e2e.yml already
//! landed): (a) cam2-painter must be provably PAINTING before the cam-pixel probe (cam1's picture
//! IS cam2-painter's HDMI, so a mid-restart painter reads as a false dead leg); (b) when the
//! sender-bounce reverify exhausts its budget, distinguish a genuine dead source from the issue-1096
//! RECEIVER wedge (strih's DistroAV never re-locks -> `received=` not advancing) and, ONLY for the
//! wedge, restart strih OBS once (headless-safe: force-kill obs64 + clear .sentinel over ssh; strih's
//! session-1 AutoHotkey64 respawns one clean genlock obs64 -- NO ssh GUI launch), then re-check once.
//!
//! All Tier-0 (no rig, no ssh, no OBS): the pure `mv_reverify_wedge_verdict`, the painter-up REMOTE
//! cmd builder (re-exec'd with fake systemctl/journalctl/fuser on PATH, the #833/#716 pattern), the
//! headless OBS-restart PowerShell (static), the `received=` reader (env-overridable), and the
//! orchestrator's decision flow (fakes for `preflight_mv_reverify` / the probe / the restart).
//! recording-e2e.sh's WIRING is a static read of the shell text (the sibling-harness model).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    manifest_dir().join("scripts/lib/mv-reverify-escalate.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib and run `snippet`; return (exit_ok, stdout_trimmed). `set -uo pipefail` (never
/// `-e`) so best-effort `|| ...` fallbacks never abort the harness. A missing lib -> the source
/// fails, the function is undefined -> empty stdout -> the assertion fails cleanly (RED).
fn run(snippet: &str) -> (bool, String) {
    let script = format!(
        "set -uo pipefail\n. \"{}\" 2>/dev/null\n{snippet}",
        lib_path().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

// ---- (b) the pure wedge verdict ----------------------------------------------------------------

#[test]
fn wedge_verdict_frozen_counter_is_a_wedge() {
    let (_ok, v) = run("mv_reverify_wedge_verdict 100 100");
    assert_eq!(
        v, "WEDGE",
        "#1093: received= did not advance (curr==prev) -> receiver wedge"
    );
}

#[test]
fn wedge_verdict_advancing_counter_is_not_a_wedge() {
    let (_ok, v) = run("mv_reverify_wedge_verdict 100 160");
    assert_eq!(
        v, "NO_WEDGE",
        "#1093: received= advanced -> frames flowing, not a wedge"
    );
}

#[test]
fn wedge_verdict_counter_reset_is_not_a_wedge() {
    // curr < prev: the cumulative counter reset (OBS restarted between samples) -> recv resumed.
    let (_ok, v) = run("mv_reverify_wedge_verdict 900 40");
    assert_eq!(
        v, "NO_WEDGE",
        "#1093: a counter reset (curr<prev) is recv resuming, never a false wedge"
    );
}

#[test]
fn wedge_verdict_absent_recv_line_is_a_wedge() {
    // curr empty (no `received=` line at all) -> "no recv".
    let (_ok, v) = run("mv_reverify_wedge_verdict 100 ''");
    assert_eq!(
        v, "WEDGE",
        "#1093: no recv line at all (empty curr) is the 'no recv' wedge"
    );
}

#[test]
fn wedge_verdict_first_reading_with_no_prior_is_not_a_wedge() {
    // prev empty but curr numeric: one sample cannot prove 'stuck'.
    let (_ok, v) = run("mv_reverify_wedge_verdict '' 100");
    assert_eq!(
        v, "NO_WEDGE",
        "#1093: a first numeric reading with no prior is not a wedge"
    );
}

#[test]
fn wedge_verdict_both_absent_is_a_wedge() {
    let (_ok, v) = run("mv_reverify_wedge_verdict '' ''");
    assert_eq!(v, "WEDGE", "#1093: no recv line on either sample -> wedge");
}

// ---- (a) painter-up proof — the REMOTE cmd builder ---------------------------------------------

#[test]
fn painter_up_cmds_reuse_the_presenter_aware_painting_signal() {
    let (_ok, text) = run("mv_reverify_painter_up_cmds");
    assert!(
        text.contains("cam2-painter.service"),
        "#1093(a): painter-up proof must key on cam2-painter.service"
    );
    assert!(text.contains("presenter: using DRM/KMS page-flip"),
        "#1093(a): must reuse the presenter-aware KMS painting signal (#863/#464), not a bare fb0 check");
    assert!(
        text.contains("vblank-locked"),
        "#1093(a): KMS painting requires the vblank-locked signal"
    );
    assert!(
        text.contains("PAINTER_UP"),
        "#1093(a): must emit PAINTER_UP on a confirmed painting signal"
    );
    assert!(
        text.contains("PAINTER_NOT_CONFIRMED"),
        "#1093(a): must emit PAINTER_NOT_CONFIRMED after the bounded budget"
    );
}

/// Re-exec the painter-up REMOTE cmds with fake systemctl/journalctl/fuser on PATH (the #833
/// pattern), simulating what the painter box would run.
fn run_painter_up(active: &str, journal: &str, fuser_ok: bool, iters: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!(
        "mv_reverify_painter_up_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let write_fake = |name: &str, body: &str| {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).unwrap();
    };
    write_fake(
        "systemctl",
        &format!("#!/usr/bin/env bash\nprintf '%s' '{active}'\n"),
    );
    // journalctl -u ... prints the fixed journal text.
    write_fake(
        "journalctl",
        &format!("#!/usr/bin/env bash\ncat <<'J'\n{journal}\nJ\n"),
    );
    write_fake(
        "fuser",
        &format!(
            "#!/usr/bin/env bash\nexit {}\n",
            if fuser_ok { 0 } else { 1 }
        ),
    );
    write_fake("pgrep", "#!/usr/bin/env bash\nexit 1\n");
    // Generate the cmds text on dev1 (real PATH), then re-exec it under the restricted fake PATH.
    let gen = format!(
        "set -uo pipefail\n. \"{}\" 2>/dev/null\nMV_REVERIFY_PAINTER_UP_ITERS={iters} mv_reverify_painter_up_cmds",
        lib_path().display()
    );
    let cmds = Command::new("bash")
        .arg("-c")
        .arg(&gen)
        .output()
        .expect("gen");
    let cmds_text = String::from_utf8_lossy(&cmds.stdout).to_string();
    let out = Command::new("/usr/bin/bash")
        .arg("-c")
        .arg(&cmds_text)
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .expect("exec cmds");
    let _ = fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn painter_up_cmds_confirm_a_genuinely_painting_kms_painter() {
    let journal =
        "obs painter: using DRM/KMS page-flip (/dev/dri/card0)\nvblank-locked at 60.000 Hz";
    let (code, out) = run_painter_up("active", journal, true, "3");
    assert_eq!(
        code, 0,
        "#1093(a): a painting KMS painter must exit 0; got out={out}"
    );
    assert!(
        out.contains("PAINTER_UP"),
        "#1093(a): expected PAINTER_UP, got {out}"
    );
}

#[test]
fn painter_up_cmds_report_not_confirmed_when_the_painter_is_dark() {
    // service active but the DRM device is NOT held (fuser fails) and no fb0 -> never painting.
    let journal = "obs painter: using DRM/KMS page-flip (/dev/dri/card0)";
    let (code, out) = run_painter_up("active", journal, false, "1");
    assert_ne!(
        code, 0,
        "#1093(a): a dark painter must exit non-zero; got out={out}"
    );
    assert!(
        out.contains("PAINTER_NOT_CONFIRMED"),
        "#1093(a): expected PAINTER_NOT_CONFIRMED, got {out}"
    );
}

// ---- (b) the headless OBS-restart PowerShell ---------------------------------------------------

#[test]
fn obs_restart_ps_is_headless_safe_kill_plus_sentinel_only() {
    let (_ok, ps) = run("mv_reverify_obs_restart_ps");
    assert!(
        ps.contains("Stop-Process") && ps.contains("obs64"),
        "#1093(b): must force-kill obs64 (session-agnostic)"
    );
    assert!(ps.contains(".sentinel"),
        "#1093(b): must clear the crash sentinels so the AHK respawn comes up clean (no Safe-Mode modal)");
    assert!(!ps.contains("Start-Process"),
        "#1093(b): must NOT Start-Process obs64 -- that is a session-1 GUI launch banned over ssh; \
         strih's NL_STARTUP.ahk owns the respawn (win-ssh-vs-mcp)");
    assert!(
        !ps.to_lowercase().contains("stop-process -name autohotkey")
            && !ps.to_lowercase().contains("stop-process autohotkey"),
        "#1093(b): must NOT stop AutoHotkey64 -- it is the respawn watcher we rely on"
    );
}

// ---- (b) the received= reader (env-overridable) ------------------------------------------------

#[test]
fn probe_received_extracts_the_newest_counter_for_the_named_source() {
    // Fake the whole ssh log read via MV_REVERIFY_RECEIVED_CMD (a stub printing raw log text).
    let stub = std::env::temp_dir().join(format!("mv_rx_stub_{}.sh", std::process::id()));
    fs::write(
        &stub,
        "#!/usr/bin/env bash\ncat <<'L'\ngenlock-fifo audit 'NDI cam1': received=100 dropped=0\ngenlock-fifo audit 'NDI cam2': received=555 dropped=0\ngenlock-fifo audit 'NDI cam1': received=740 dropped=0\nL\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();
    let (_ok, v) = run(&format!(
        "MV_REVERIFY_RECEIVED_CMD='{}' mv_reverify_probe_received 10.0.0.1 'NDI cam1'",
        stub.display()
    ));
    let _ = fs::remove_file(&stub);
    assert_eq!(
        v, "740",
        "#1093(b): must extract the NEWEST received= for the named source (not cam2's)"
    );
}

#[test]
fn probe_received_is_empty_when_the_source_has_no_audit_line() {
    let stub = std::env::temp_dir().join(format!("mv_rx_stub_none_{}.sh", std::process::id()));
    fs::write(&stub, "#!/usr/bin/env bash\nprintf 'no audit here\\n'\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();
    let (_ok, v) = run(&format!(
        "MV_REVERIFY_RECEIVED_CMD='{}' mv_reverify_probe_received 10.0.0.1 'NDI cam1'",
        stub.display()
    ));
    let _ = fs::remove_file(&stub);
    assert_eq!(
        v, "",
        "#1093(b): an absent audit line -> empty reading (drives the 'no recv' wedge)"
    );
}

// ---- (b) the orchestrator decision flow --------------------------------------------------------

/// Drive `mv_reverify_or_escalate` with fakes: a `preflight_mv_reverify` whose success is driven by
/// a counter file, an env-stubbed received= reader, and an env-stubbed OBS restart. Returns
/// (stdout+stderr, rc).
fn run_orchestrator(preflight_body: &str, received_cmd: &str) -> (String, i32) {
    let restart_log = std::env::temp_dir().join(format!("mv_restart_{}.log", nanos()));
    let restart_stub = std::env::temp_dir().join(format!("mv_restart_{}.sh", nanos()));
    fs::write(
        &restart_stub,
        format!(
            "#!/usr/bin/env bash\necho RESTARTED >> '{}'\necho RESTARTED\n",
            restart_log.display()
        ),
    )
    .unwrap();
    make_exec(&restart_stub);
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\n\
         HERE='{here}'\nSTRIH=10.0.0.9\nALL_CAMBOX=1\nSTRIH_USER=x\nSTRIH_PW=x\n\
         MV_REVERIFY_RECEIVED_CMD='{rx}'\nMV_REVERIFY_OBS_RESTART_CMD='{restart}'\n\
         MV_REVERIFY_SWEEP_CMD=/bin/true\nMV_REVERIFY_REOPEN_MV_CMD=/bin/true\n\
         MV_REVERIFY_HEAL_WAIT_CMD=/bin/true\nCAMERA_ACTIVE_SET='cam1 cam2 cam3'\n\
         MV_REVERIFY_WEDGE_SAMPLE_GAP_S=0\nMV_REVERIFY_OBS_WS_WAIT_ITERS=0\nMV_REVERIFY_OBS_WS_WAIT_GAP_S=0\n\
         {preflight}\n\
         mv_reverify_or_escalate cam1 1; echo RC=$?\n",
        lib = lib_path().display(),
        here = manifest_dir().join("scripts").display(),
        rx = received_cmd,
        restart = restart_stub.display(),
        preflight = preflight_body,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("orchestrator");
    let _ = fs::remove_file(&restart_stub);
    let _ = fs::remove_file(&restart_log);
    let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    let rc = extract_rc(&combined);
    (combined, rc)
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn make_exec(p: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

fn extract_rc(s: &str) -> i32 {
    s.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("RC="))
        .and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(-999)
}

/// A `received=` stub that ALWAYS prints a frozen counter (same value every call) -> WEDGE.
fn frozen_rx_stub() -> String {
    let p = std::env::temp_dir().join(format!("mv_rx_frozen_{}.sh", nanos()));
    fs::write(
        &p,
        "#!/usr/bin/env bash\nprintf \"genlock-fifo audit 'NDI cam1': received=500 dropped=0\\n\"\n",
    )
    .unwrap();
    make_exec(&p);
    p.display().to_string()
}

/// A `received=` stub that prints an ADVANCING counter (grows each call) -> NO_WEDGE.
fn advancing_rx_stub() -> String {
    let cnt = std::env::temp_dir().join(format!("mv_rx_cnt_{}", nanos()));
    let p = std::env::temp_dir().join(format!("mv_rx_adv_{}.sh", nanos()));
    fs::write(
        &p,
        format!(
            "#!/usr/bin/env bash\nn=$(cat '{c}' 2>/dev/null || echo 100)\nn=$((n+50))\necho $n > '{c}'\nprintf \"genlock-fifo audit 'NDI cam1': received=$n dropped=0\\n\"\n",
            c = cnt.display()
        ),
    )
    .unwrap();
    make_exec(&p);
    p.display().to_string()
}

#[test]
fn orchestrator_passes_straight_through_when_the_leg_is_live() {
    // preflight succeeds immediately -> no probing, no restart.
    let (out, rc) = run_orchestrator("preflight_mv_reverify() { return 0; }", &frozen_rx_stub());
    assert_eq!(
        rc, 0,
        "#1093: a live leg returns 0 with no escalation; out=\n{out}"
    );
    assert!(
        !out.contains("RESTARTED"),
        "#1093: no OBS restart when the leg is live; out=\n{out}"
    );
}

#[test]
fn orchestrator_restarts_obs_once_and_recovers_on_a_receiver_wedge() {
    // preflight FAILS on the 1st call, SUCCEEDS on the 2nd (the post-restart re-check); received=
    // frozen -> WEDGE -> restart strih OBS once -> re-check passes.
    let cnt = std::env::temp_dir().join(format!("mv_pf_{}", nanos()));
    let _ = fs::remove_file(&cnt);
    let preflight = format!(
        "preflight_mv_reverify() {{ n=$(cat '{c}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{c}'; [ $n -ge 2 ]; }}",
        c = cnt.display()
    );
    let (out, rc) = run_orchestrator(&preflight, &frozen_rx_stub());
    let _ = fs::remove_file(&cnt);
    assert!(
        out.contains("RESTARTED"),
        "#1093(b): a receiver wedge must restart strih OBS; out=\n{out}"
    );
    assert_eq!(
        rc, 0,
        "#1093(b): the leg must recover after the restart + re-check; out=\n{out}"
    );
}

#[test]
fn orchestrator_does_not_restart_obs_when_recv_is_advancing() {
    // preflight always fails, but received= is ADVANCING -> NOT a wedge (source dead, not receiver)
    // -> NO OBS restart, fail loud.
    let (out, rc) = run_orchestrator(
        "preflight_mv_reverify() { return 1; }",
        &advancing_rx_stub(),
    );
    assert!(!out.contains("RESTARTED"),
        "#1093(b): an advancing received= is a dead SOURCE, never a receiver wedge -- no OBS restart; out=\n{out}");
    assert_eq!(
        rc, 1,
        "#1093(b): a genuinely dead leg fails loud (rc=1); out=\n{out}"
    );
}

#[test]
fn orchestrator_restarts_obs_at_most_once_per_run() {
    // preflight always fails + frozen received= (WEDGE). First escalation restarts; but with the
    // MV_REVERIFY_OBS_RESTARTED guard pre-set, a SECOND leg's escalation must NOT restart again.
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\n\
         HERE='{here}'\nSTRIH=10.0.0.9\nALL_CAMBOX=1\nSTRIH_USER=x\nSTRIH_PW=x\n\
         MV_REVERIFY_RECEIVED_CMD='{rx}'\nMV_REVERIFY_OBS_RESTART_CMD='{restart}'\n\
         MV_REVERIFY_HEAL_WAIT_CMD=/bin/true\n\
         MV_REVERIFY_WEDGE_SAMPLE_GAP_S=0\nMV_REVERIFY_OBS_WS_WAIT_ITERS=1\nMV_REVERIFY_OBS_WS_WAIT_GAP_S=0\n\
         MV_REVERIFY_OBS_RESTARTED=1\n\
         preflight_mv_reverify() {{ return 1; }}\n\
         mv_reverify_or_escalate cam3 3; echo RC=$?\n",
        lib = lib_path().display(),
        here = manifest_dir().join("scripts").display(),
        rx = frozen_rx_stub(),
        restart = "/bin/echo",
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not restarting again") || combined.contains("prior strih-OBS restart"),
        "#1093(b): once OBS was restarted this run, a later wedge must NOT restart it again; out=\n{combined}"
    );
    assert!(
        extract_rc(&combined) == 1,
        "#1093(b): a still-wedged leg after the one restart fails loud; out=\n{combined}"
    );
}

/// A `received=` stub that prints NOTHING -> the log READ failed (empty raw), distinct from a
/// read that succeeded but has no audit line.
fn empty_rx_stub() -> String {
    let p = std::env::temp_dir().join(format!("mv_rx_empty_{}.sh", nanos()));
    fs::write(&p, "#!/usr/bin/env bash\nexit 0\n").unwrap();
    make_exec(&p);
    p.display().to_string()
}

#[test]
fn orchestrator_does_not_restart_obs_when_the_strih_log_read_fails() {
    // #1093 review finding 3: both received= reads empty = the LOG READ failed (ssh blip / log
    // absent), NOT "no recv". Must fail loud WITHOUT force-killing strih (absence-of-evidence).
    let (out, rc) = run_orchestrator("preflight_mv_reverify() { return 1; }", &empty_rx_stub());
    assert!(
        !out.contains("RESTARTED"),
        "#1093(3): an unreadable strih log must NEVER force-kill strih OBS; out=\n{out}"
    );
    assert!(
        out.contains("READ_FAIL") || out.contains("could NOT read"),
        "#1093(3): must report READ_FAIL, not a wedge; out=\n{out}"
    );
    assert_eq!(
        rc, 1,
        "#1093(3): a read failure fails loud (rc=1); out=\n{out}"
    );
}

#[test]
fn orchestrator_fails_loud_without_killing_when_ahk_absent() {
    // #1093 review finding 2: a WEDGE whose restart reports MV_REVERIFY_NO_AHK (the respawn watcher
    // is absent) must fail loud WITHOUT having killed obs64 (never leave strih OBS down).
    let no_ahk_stub = std::env::temp_dir().join(format!("mv_noahk_{}.sh", nanos()));
    fs::write(
        &no_ahk_stub,
        "#!/usr/bin/env bash\necho MV_REVERIFY_NO_AHK\n",
    )
    .unwrap();
    make_exec(&no_ahk_stub);
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\n\
         HERE='{here}'\nSTRIH=10.0.0.9\nALL_CAMBOX=1\nSTRIH_USER=x\nSTRIH_PW=x\n\
         MV_REVERIFY_RECEIVED_CMD='{rx}'\nMV_REVERIFY_OBS_RESTART_CMD='{restart}'\n\
         MV_REVERIFY_SWEEP_CMD=/bin/true\nMV_REVERIFY_HEAL_WAIT_CMD=/bin/true\n\
         MV_REVERIFY_WEDGE_SAMPLE_GAP_S=0\nMV_REVERIFY_OBS_WS_WAIT_ITERS=0\nMV_REVERIFY_OBS_WS_WAIT_GAP_S=0\n\
         preflight_mv_reverify() {{ return 1; }}\n\
         mv_reverify_or_escalate cam1 1; echo RC=$?\n",
        lib = lib_path().display(),
        here = manifest_dir().join("scripts").display(),
        rx = frozen_rx_stub(),
        restart = no_ahk_stub.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run");
    let _ = fs::remove_file(&no_ahk_stub);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("ABSENT") || combined.contains("AutoHotkey64"),
        "#1093(2): must report the AHK watcher is absent; out=\n{combined}"
    );
    assert!(
        !combined.contains("recovered after"),
        "#1093(2): must NOT claim recovery when the restart was skipped; out=\n{combined}"
    );
    assert_eq!(
        extract_rc(&combined),
        1,
        "#1093(2): fail loud (rc=1); out=\n{combined}"
    );
}

#[test]
fn obs_restart_ps_guards_on_ahk_presence_before_killing() {
    // #1093 review finding 2: the PS must CHECK AutoHotkey64 is alive and `exit 2` (MV_REVERIFY_NO_AHK)
    // BEFORE the Stop-Process obs64 -- never kill obs64 when nothing will respawn it.
    let (_ok, ps) = run("mv_reverify_obs_restart_ps");
    assert!(
        ps.contains("Get-Process AutoHotkey64")
            && ps.contains("MV_REVERIFY_NO_AHK")
            && ps.contains("exit 2"),
        "#1093(2): the restart PS must guard on AutoHotkey64 presence and exit 2 when absent"
    );
    let guard = ps.find("MV_REVERIFY_NO_AHK").expect("guard present");
    let kill = ps.find("Stop-Process").expect("kill present");
    assert!(
        guard < kill,
        "#1093(2): the AHK-presence guard must precede the obs64 kill (never kill first, check later)"
    );
}

// ---- recording-e2e.sh WIRING (static reads) ----------------------------------------------------

#[test]
fn recording_e2e_sources_the_escalate_lib() {
    let s = recording_e2e_text();
    assert!(
        s.contains("lib/mv-reverify-escalate.sh"),
        "#1093: recording-e2e.sh must source the escalation lib"
    );
}

#[test]
fn recording_e2e_deploy_sites_use_the_guarded_reverify() {
    let s = recording_e2e_text();
    assert!(s.contains(r#"mv_reverify_or_escalate "$CAMERA_NAME" "${CAMERA_NAME#cam}""#),
        "#1093: the cam1 deploy site must call the guarded reverify (painter-order + wedge escalation)");
    assert!(
        s.contains(r#"mv_reverify_or_escalate "$_cn" "${_cn#cam}""#),
        "#1093: the ALL_CAMBOX loop deploy site must call the guarded reverify"
    );
    // The bare `preflight_mv_reverify … || exit 1` deploy-time abort must be GONE from the call
    // sites (the guard now owns the exit-1 decision). The function itself + the cleanup wrapper's
    // own use stay.
    assert!(
        !s.contains(r#"preflight_mv_reverify "$CAMERA_NAME" "${CAMERA_NAME#cam}" || exit 1"#),
        "#1093: the raw cam1 reverify||exit must be replaced by the guarded call"
    );
}

#[test]
fn painter_up_wait_runs_once_before_the_cam1_probe() {
    let s = recording_e2e_text();
    let waits: Vec<_> = s.match_indices("mv_reverify_painter_up_wait").collect();
    assert_eq!(
        waits.len(),
        1,
        "#1093(a): the painter-up wait fires exactly ONCE (before cam1's probe) -- NOT before the \
         ALL_CAMBOX loop, where the painter is deliberately stopped; found {}",
        waits.len()
    );
    let wait_at = waits[0].0;
    let cam1_call = s
        .find(r#"mv_reverify_or_escalate "$CAMERA_NAME""#)
        .expect("#1093: cam1 guarded reverify call must exist");
    assert!(
        wait_at < cam1_call,
        "#1093(a): the painter-up wait must run BEFORE the cam1 cam-pixel probe"
    );
}

#[test]
fn cleanup_reverify_stays_warn_only_and_never_escalates() {
    // The cleanup wrapper must keep calling the shared preflight_mv_reverify directly (WARN-only),
    // never the deploy-time escalation (which force-kills strih OBS -- forbidden in the EXIT trap).
    let s = recording_e2e_text();
    let start = s
        .find("cleanup_mv_reverify_active_boxes() {")
        .expect("cleanup wrapper must exist");
    let end = s[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("wrapper closes");
    let wrapper = &s[start..end];
    assert!(!wrapper.contains("mv_reverify_or_escalate"),
        "#1093: the cleanup wrapper must NEVER call the OBS-restart escalation (WARN-only trap safety)");
}

#[test]
fn orchestrator_restart_budget_allows_a_second_restart_within_cap() {
    // #1093 follow-up (issue 1096 live rate, 2026-08-17): with today's per-bounce wedge rate a
    // single restart per RUN cannot carry a run whose deploy bounces 3+ senders — the budget is
    // now a COUNTER (MV_REVERIFY_OBS_RESTART_MAX, default 3). Two successive wedged legs must BOTH
    // get a restart while under the cap; the legacy MV_REVERIFY_OBS_RESTARTED=1 kill-switch stays
    // honored (previous test). The restart stub records invocations to a file.
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("restarts.log");
    let stub = dir.path().join("restart-stub.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/bash\necho x >> {}\n", marker.display()),
    )
    .expect("write stub");
    let mut perms = std::fs::metadata(&stub).expect("meta").permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\n\
         HERE='{here}'\nSTRIH=10.0.0.9\nALL_CAMBOX=1\nSTRIH_USER=x\nSTRIH_PW=x\n\
         MV_REVERIFY_RECEIVED_CMD='{rx}'\nMV_REVERIFY_OBS_RESTART_CMD='{restart}'\n\
         MV_REVERIFY_SWEEP_CMD='/bin/true'\nMV_REVERIFY_REOPEN_MV_CMD='/bin/true'\n\
         MV_REVERIFY_HEAL_WAIT_CMD='/bin/true'\nCAMERA_ACTIVE_SET='cam2 cam3'\n\
         MV_REVERIFY_WEDGE_SAMPLE_GAP_S=0\nMV_REVERIFY_OBS_WS_WAIT_ITERS=1\nMV_REVERIFY_OBS_WS_WAIT_GAP_S=0\n\
         preflight_mv_reverify() {{ return 1; }}\n\
         mv_reverify_or_escalate cam2 2; echo RC1=$?\n\
         mv_reverify_or_escalate cam3 3; echo RC2=$?\n",
        lib = lib_path().display(),
        here = manifest_dir().join("scripts").display(),
        rx = frozen_rx_stub(),
        restart = stub.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let restarts = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        restarts.lines().count(),
        2,
        "#1093/#1096: two wedged legs under the cap must EACH get a strih-OBS restart (got {} restarts); out=\n{combined}",
        restarts.lines().count()
    );
}

// ---- #1098: restore strih's operator Multiview projector after the force-kill restart ----------

#[test]
fn orchestrator_reopens_strih_multiview_after_a_receiver_wedge_restart() {
    // #1098: a force-kill restart of strih OBS (the receiver-wedge escalation) leaves the operator
    // without their standing FULLSCREEN Multiview projector -- strih's SaveProjectors=true but
    // SavedProjectors is EMPTY, and a force-kill never repopulates it, so OBS restores nothing and
    // the AHK respawn only re-launches obs64 (no projector). After the restart + WS-return wait +
    // burn sweep-off, mv_reverify_or_escalate MUST re-open the Multiview projector over OBS WS
    // (mv_reverify_reopen_multiview_run -> obs_phase2.py open-multiview), overridable for tests via
    // MV_REVERIFY_REOPEN_MV_CMD, WARN-only. This drives a receiver-wedge recovery and asserts the
    // re-open command actually ran.
    let dir = tempfile::tempdir().expect("tempdir");
    let reopen_log = dir.path().join("reopen.log");
    let reopen_stub = dir.path().join("reopen-stub.sh");
    std::fs::write(
        &reopen_stub,
        format!(
            "#!/usr/bin/env bash\necho \"REOPENED $1\" >> {}\n",
            reopen_log.display()
        ),
    )
    .expect("write reopen stub");
    make_exec(&reopen_stub);

    let restart_stub = dir.path().join("restart-stub.sh");
    std::fs::write(&restart_stub, "#!/usr/bin/env bash\necho RESTARTED\n").expect("write restart");
    make_exec(&restart_stub);

    // preflight FAILS on the 1st call, SUCCEEDS on the 2nd (the post-restart re-check).
    let cnt = dir.path().join("pf.cnt");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\n\
         HERE='{here}'\nSTRIH=10.0.0.9\nALL_CAMBOX=1\nSTRIH_USER=x\nSTRIH_PW=x\n\
         MV_REVERIFY_RECEIVED_CMD='{rx}'\nMV_REVERIFY_OBS_RESTART_CMD='{restart}'\n\
         MV_REVERIFY_SWEEP_CMD='/bin/true'\nMV_REVERIFY_REOPEN_MV_CMD='{reopen}'\n\
         MV_REVERIFY_HEAL_WAIT_CMD='/bin/true'\nCAMERA_ACTIVE_SET='cam1 cam2 cam3'\n\
         MV_REVERIFY_WEDGE_SAMPLE_GAP_S=0\nMV_REVERIFY_OBS_WS_WAIT_ITERS=0\nMV_REVERIFY_OBS_WS_WAIT_GAP_S=0\n\
         preflight_mv_reverify() {{ n=$(cat '{c}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{c}'; [ $n -ge 2 ]; }}\n\
         mv_reverify_or_escalate cam1 1; echo RC=$?\n",
        lib = lib_path().display(),
        here = manifest_dir().join("scripts").display(),
        rx = frozen_rx_stub(),
        restart = restart_stub.display(),
        reopen = reopen_stub.display(),
        c = cnt.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = extract_rc(&combined);
    let reopened = std::fs::read_to_string(&reopen_log).unwrap_or_default();
    assert!(
        reopened.contains("REOPENED 10.0.0.9"),
        "#1098: after the force-kill restart the orchestrator MUST re-open strih's Multiview \
         projector (mv_reverify_reopen_multiview_run $STRIH); reopen.log=\n{reopened}\nout=\n{combined}"
    );
    assert_eq!(
        rc, 0,
        "#1098: the leg still recovers after the restart + re-check (the re-open is WARN-only); out=\n{combined}"
    );
}

#[test]
fn reopen_multiview_run_is_warn_only_and_never_fails_the_run() {
    // #1098: a failing re-open (nonzero exit) must NOT fail -- the operator-facing restore is
    // best-effort; the leg recovery already succeeded projector-independently (positive warm-settle).
    let (_ok, out) = run(
        "MV_REVERIFY_REOPEN_MV_CMD='/bin/false' mv_reverify_reopen_multiview_run 10.0.0.9; echo RC=$?",
    );
    assert!(
        out.lines().any(|l| l.trim() == "RC=0"),
        "#1098: mv_reverify_reopen_multiview_run must be WARN-only (return 0) even when the re-open \
         command fails; out=\n{out}"
    );
}

#[test]
fn orchestrator_reopens_multiview_after_sweep_off_in_the_wiring() {
    // Static ordering guard: in mv_reverify_or_escalate the Multiview re-open call must sit AFTER
    // the burn sweep-off (both are post-restart, session-agnostic WS ops), so the fresh OBS's burn
    // is cleared before the operator's multiview is restored.
    let s = std::fs::read_to_string(lib_path()).expect("read lib");
    let esc = s
        .find("mv_reverify_or_escalate()")
        .expect("orchestrator fn present");
    let body = &s[esc..];
    let sweep = body
        .find("MV_REVERIFY_SWEEP_CMD")
        .expect("#1098: sweep-off must exist in the orchestrator");
    let reopen = body.find("mv_reverify_reopen_multiview_run").expect(
        "#1098: the orchestrator must call mv_reverify_reopen_multiview_run after the restart",
    );
    assert!(
        sweep < reopen,
        "#1098: the Multiview re-open must run AFTER the burn sweep-off in mv_reverify_or_escalate"
    );
}
