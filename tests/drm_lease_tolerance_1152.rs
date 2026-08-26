//! Issue 1152 M4 — provisioning + preflight facets + a DRM-lease-tolerant wrapper.
//!
//! M1+M2 proved the in-OBS DRM-lease output mechanism (lease + solid flip; Program dma-buf
//! scanout), but PERMANENT deployment was impossible: (1) `imag-obs-start.sh` runs
//! `imag_scenes.py --projector` unconditionally under `set -euo pipefail`, and `projector()`
//! sys.exits when no HDMI monitor is in the X layout — with the connector leased out of X the
//! unit exit-1s and systemd CRASH-LOOPS OBS (the live 2026-08-26 M1 runbook gotcha, restart
//! counter 5→8); (2) nothing guarded the ENABLED state (config armed + no scanout = a silently
//! grey projector) while the `hdmi_primary` facet actively FALSE-DRIFTed the E2E `[0/8]`
//! preflight in lease mode (HDMI out of X ⇒ panel primary ⇒ "primary is eDP-1 not HDMI");
//! (3) the DEFAULT-OFF `~/.camera-box/drm-output.json` invariant (provisioning must NEVER
//! auto-enable it) was unlocked by any test, and no acceptance check proved the box's INSTALLED
//! wrapper generation tolerates the lease (the #840 "hand-placed hides a provisioning gap"
//! class).
//!
//! Std-only (Tier-0): sources the REAL `scripts/lib/imag-display-path.sh` and runs its pure
//! verdict over fixtures (the same harness shape as tests/harness_imag_display_path_780.rs),
//! plus static anchors over the wrapper / seeder / verify / setup scripts. The python halves
//! (`drm_output_lease_enabled` truth table + the lease-mode `projector()` flow) live in
//! tests/python/test_imag_scenes_drm_lease_1152.py.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/imag-display-path.sh")
}

fn read_repo(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Source the REAL display-path lib and run `body` against its pure functions.
fn run_lib_sourced(body: &str) -> (i32, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

fn verdict(gather: &str) -> Vec<String> {
    let (rc, out) = run_lib_sourced(&format!(
        "imag_display_path_verdict {}",
        shell_quote(gather)
    ));
    assert_eq!(rc, 0, "verdict must exit 0 (pure); stdout={out:?}");
    out.lines().map(str::to_string).collect()
}

fn facet_line(lines: &[String], facet: &str) -> String {
    lines
        .iter()
        .find(|l| l.starts_with(&format!("{facet}|")))
        .unwrap_or_else(|| panic!("no {facet} row printed in: {lines:?}"))
        .clone()
}

fn status_of(lines: &[String], facet: &str) -> String {
    facet_line(lines, facet)
        .split('|')
        .nth(1)
        .unwrap_or("")
        .to_string()
}

/// A fully-clean gather with the OTHER facets healthy, parameterised on the DRM-output keys.
fn gather_with_drm(drm_keys: &str) -> String {
    format!(
        "PICOM_PGREP|ok\nPICOM_PROC|\nPICOM_SERVICE|disabled\n\
         XRANDR|ok\nPRIMARY_OUTPUT|HDMI-1\n\
         MAXPERF_APPLICABLE|1\nMAXPERF_MIN|1400\nMAXPERF_RP0|1400\n\
         MAXPERF_ENABLED|enabled\nMAXPERF_ACTIVE|active\n\
         TAPCONF|present\nTAPCONF_TAPPING|on\n{drm_keys}"
    )
}

// ================================================================================================
// A. the drm_output facet (shared lib — flows into drift-guard check 10, the E2E [0/8]
//    preflight and verify-imag (z) through their existing generic loops)
// ================================================================================================

/// The fleet default: config ABSENT → OK-dormant, so drift-guard / the [0/8] preflight /
/// verify (z) never false-abort on a box that simply does not run the DRM output.
#[test]
fn drm_output_ok_dormant_when_config_absent_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|absent\nDRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|none",
    );
    let lines = verdict(&g);
    let l = facet_line(&lines, "drm_output");
    assert_eq!(status_of(&lines, "drm_output"), "OK", "{l}");
    assert!(
        l.contains("dormant"),
        "the OK detail must name the dormant default: {l}"
    );
}

/// A present-but-disabled config is equally dormant (explicit "enabled": false).
#[test]
fn drm_output_ok_dormant_when_config_disabled_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|false\nDRM_OUTPUT_PROGRAM|true\n\
         DRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|none",
    );
    let lines = verdict(&g);
    assert_eq!(
        status_of(&lines, "drm_output"),
        "OK",
        "{:?}",
        facet_line(&lines, "drm_output")
    );
}

/// ENABLED + the current OBS log carries `program scanout LIVE` = the healthy armed state.
#[test]
fn drm_output_ok_when_enabled_and_scanout_live_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|true\n\
         DRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|live",
    );
    let lines = verdict(&g);
    let l = facet_line(&lines, "drm_output");
    assert_eq!(status_of(&lines, "drm_output"), "OK", "{l}");
    assert!(
        l.contains("LIVE") || l.contains("scanout"),
        "the OK detail must name the live scanout proof: {l}"
    );
}

/// ENABLED but no scanout marker in the current OBS log — the projector is silently NOT carrying
/// the Program. Fail loud (the M4 scope's exact DRIFT case).
#[test]
fn drm_output_drift_when_enabled_without_live_marker_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|true\n\
         DRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|none",
    );
    let lines = verdict(&g);
    assert_eq!(
        status_of(&lines, "drm_output"),
        "DRIFT",
        "{:?}",
        facet_line(&lines, "drm_output")
    );
}

/// ENABLED + the log shows the bind FAILED → the HDMI shows the solid fallback, not the Program.
#[test]
fn drm_output_drift_when_enabled_bind_failed_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|true\n\
         DRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|bind-failed",
    );
    let lines = verdict(&g);
    let l = facet_line(&lines, "drm_output");
    assert_eq!(status_of(&lines, "drm_output"), "DRIFT", "{l}");
    assert!(
        l.contains("bind"),
        "the DRIFT detail must name the failed bind: {l}"
    );
}

/// ENABLED in the M1 solid diagnostic mode ("program": false) is NOT a production state — cam2's
/// grabber taps the imag HDMI, so a grey test pattern would wreck a data run. Fail loud.
#[test]
fn drm_output_drift_when_enabled_solid_diagnostic_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|false\n\
         DRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|solid-only",
    );
    let lines = verdict(&g);
    let l = facet_line(&lines, "drm_output");
    assert_eq!(status_of(&lines, "drm_output"), "DRIFT", "{l}");
    assert!(
        l.contains("solid") || l.contains("diagnostic"),
        "the DRIFT detail must name the diagnostic solid mode: {l}"
    );
}

/// ENABLED but the box has NO readable OBS log at all (gathered: the log dir is empty) — there is
/// no proof the Program reaches the projector. Fail loud, never a silent pass.
#[test]
fn drm_output_drift_when_enabled_and_no_obs_log_files_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|true\n\
         DRM_OUTPUT_LOG|none",
    );
    let lines = verdict(&g);
    assert_eq!(
        status_of(&lines, "drm_output"),
        "DRIFT",
        "{:?}",
        facet_line(&lines, "drm_output")
    );
}

/// Backward compat / two-tier: an old or hiccuped gather with NO DRM keys at all reads UNKNOWN —
/// never a false OK, never a false DRIFT (the #833 discipline every facet in this lib follows).
#[test]
fn drm_output_unknown_when_not_gathered_1152() {
    let g = gather_with_drm("");
    let lines = verdict(&g);
    assert_eq!(
        status_of(&lines, "drm_output"),
        "UNKNOWN",
        "{:?}",
        facet_line(&lines, "drm_output")
    );
}

/// Partial gather (config keys read, the SSH died before the log block): ENABLED with the log
/// state UNREAD is UNKNOWN, not DRIFT — "not gathered" is never "proven missing".
#[test]
fn drm_output_unknown_when_log_block_not_gathered_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|true",
    );
    let lines = verdict(&g);
    assert_eq!(
        status_of(&lines, "drm_output"),
        "UNKNOWN",
        "{:?}",
        facet_line(&lines, "drm_output")
    );
}

// ================================================================================================
// B. hdmi_primary must be LEASE-AWARE — with the DRM output enabled the HDMI connector is OUT of
//    the X layout BY DESIGN, so a panel primary is correct there (the pre-M4 false DRIFT aborted
//    the whole [0/8] preflight in lease mode)
// ================================================================================================

#[test]
fn hdmi_primary_ok_in_lease_mode_with_panel_primary_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|present\nDRM_OUTPUT_ENABLED|true\nDRM_OUTPUT_PROGRAM|true\n\
         DRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|live",
    )
    .replace("PRIMARY_OUTPUT|HDMI-1", "PRIMARY_OUTPUT|eDP-1");
    let lines = verdict(&g);
    let l = facet_line(&lines, "hdmi_primary");
    assert_eq!(
        status_of(&lines, "hdmi_primary"),
        "OK",
        "lease mode: a panel primary is correct BY DESIGN, never a DRIFT: {l}"
    );
    assert!(
        l.contains("leased") || l.contains("lease"),
        "the OK detail must explain the lease mode: {l}"
    );
}

/// The dormant polarity is UNCHANGED: config absent + panel primary stays the issue-1146 DRIFT.
#[test]
fn hdmi_primary_panel_primary_stays_drift_when_dormant_1152() {
    let g = gather_with_drm(
        "DRM_OUTPUT_CONFIG|absent\nDRM_OUTPUT_LOG|present\nDRM_OUTPUT_SCANOUT|none",
    )
    .replace("PRIMARY_OUTPUT|HDMI-1", "PRIMARY_OUTPUT|eDP-1");
    let lines = verdict(&g);
    assert_eq!(
        status_of(&lines, "hdmi_primary"),
        "DRIFT",
        "dormant boxes keep the issue-1146 projector-primary doctrine: {:?}",
        facet_line(&lines, "hdmi_primary")
    );
}

// ================================================================================================
// C. the gather snippet collects the DRM-output state (config + the newest OBS log's marker)
// ================================================================================================

#[test]
fn gather_snippet_collects_the_drm_output_state_1152() {
    let (rc, snippet) = run_lib_sourced("imag_display_path_gather_remote_snippet");
    assert_eq!(rc, 0);
    for needle in [
        "drm-output.json",
        "DRM_OUTPUT_CONFIG|absent",
        "DRM_OUTPUT_ENABLED|",
        "DRM_OUTPUT_SCANOUT|",
        "drm-output: program scanout LIVE",
        "drm-output: program bind FAILED",
    ] {
        assert!(
            snippet.contains(needle),
            "gather snippet must collect `{needle}`:\n{snippet}"
        );
    }
    // #1183/#1184: OBS logs carry raw invalid-UTF-8 bytes; an ASCII marker grep in a UTF-8 locale
    // can MISS a present marker. The log greps must be byte-literal.
    assert!(
        snippet.contains("LC_ALL=C grep -a"),
        "the OBS-log marker greps must be LC_ALL=C grep -a (the mojibake net):\n{snippet}"
    );
}

// ================================================================================================
// D. the wrapper (imag-obs-start.sh) — lease mode is detected BEFORE the OBS launch, the
//    connector leaves the X layout (the idle-connector lease precondition), and everything is
//    LOUD + best-effort (never a new unit-abort path)
// ================================================================================================

#[test]
fn imag_obs_start_gains_the_lease_mode_branch_1152() {
    let body = read_repo("scripts/imag-obs-start.sh");
    for needle in ["DRM_LEASE_MODE=0", "DRM_LEASE_MODE=1", "drm-output.json"] {
        assert!(
            body.contains(needle),
            "imag-obs-start.sh must contain `{needle}`"
        );
    }
    // detection + xrandr --off sit AFTER the import preflight and BEFORE the OBS launch
    let preflight = body
        .find("FAIL: imag_scenes import preflight")
        .expect("the import preflight must stay");
    let detect = body
        .find("DRM_LEASE_MODE=0")
        .expect("the dormant default assignment must exist");
    let launch = body
        .find("obs --disable-shutdown-check &")
        .expect("the launch line must stay");
    assert!(
        preflight < detect && detect < launch,
        "lease detection must run after the import preflight and before the launch \
         (preflight={preflight} detect={detect} launch={launch})"
    );
    let off = body
        .find("xrandr --output \"$DRM_CONNECTOR\" --off")
        .expect("the enabled branch must take the config's connector out of the X layout");
    assert!(
        detect < off && off < launch,
        "the xrandr --off (idle-connector lease precondition) must sit inside the lease branch, \
         before the launch (detect={detect} off={off} launch={launch})"
    );
}

#[test]
fn imag_obs_start_lease_mode_is_loud_and_best_effort_1152() {
    let body = read_repo("scripts/imag-obs-start.sh");
    assert!(
        body.contains("drm-lease mode ENABLED"),
        "the lease branch must announce itself LOUDLY in /tmp/imag-obs-start.log — never a \
         silent skip"
    );
    assert!(
        body.contains("WARN #1152"),
        "a failed xrandr --off must WARN loudly and continue (never abort the unit — the \
         issue-866 start-path discipline), naming the consequence"
    );
}

/// The dormant path stays byte-identical in behaviour: the projector seeding call itself is
/// UNCONDITIONAL (the tolerance lives in imag_scenes.py's own lease-mode branch, which every
/// caller — unit boot, operator menu, watchdog relaunch, verify repopulate — inherits).
#[test]
fn imag_obs_start_projector_call_stays_unconditional_1152() {
    let body = read_repo("scripts/imag-obs-start.sh");
    assert!(
        body.contains(r#"python3 "$SCN" --host 127.0.0.1 --projector"#),
        "the projector seeding call must stay — lease tolerance lives in imag_scenes.py, \
         not in a wrapper-side skip of the seeding"
    );
    assert!(
        body.contains("OK: OBS bezi"),
        "the deploy-fleet render-tick verify greps /tmp/imag-obs-start.log for this exact \
         marker — it must survive in BOTH modes"
    );
}

// ================================================================================================
// E. the seeder (imag_scenes.py) — projector() consults the BOX's drm-output config BEFORE the
//    hard "no HDMI monitor" exit, and the lease branch opens ONLY the panel Multiview
// ================================================================================================

#[test]
fn imag_scenes_projector_consults_lease_mode_before_the_hdmi_fail_exit_1152() {
    let s = read_repo("scripts/imag_scenes.py");
    let call = s
        .find("if drm_output_lease_enabled(")
        .expect("projector() must consult drm_output_lease_enabled(...)");
    let fail = s
        .find("FAIL: no HDMI projector monitor detected")
        .expect("the dormant-mode fail-exit must stay for the genuinely-unplugged case");
    assert!(
        call < fail,
        "the lease check must run BEFORE the no-HDMI fail exit (call={call} fail={fail}) — \
         otherwise an armed DRM output crash-loops the unit (the 2026-08-26 M1 gotcha)"
    );
}

#[test]
fn imag_scenes_lease_branch_opens_only_the_panel_multiview_and_never_exits_1152() {
    let s = read_repo("scripts/imag_scenes.py");
    let start = s
        .find("def drm_output_lease_enabled(")
        .expect("the pure config classifier must exist");
    let end = s
        .find("def projector(")
        .expect("projector() must still exist");
    assert!(
        start < end,
        "the lease helpers must be defined above projector()"
    );
    let region = &s[start..end];
    assert!(
        region.contains("OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"),
        "the lease branch must open the panel MULTIVIEW projector"
    );
    assert!(
        !region.contains("OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"),
        "the lease branch must NEVER open an X Program projector — the Program page-flips on \
         the DRM-leased connector"
    );
    assert!(
        !region.contains("sys.exit"),
        "nothing in the lease-mode helpers may exit non-zero — this code runs on the \
         supervised OBS start path (the issue-866 crash-loop discipline)"
    );
    assert!(
        region.contains("drm-lease mode ENABLED"),
        "the lease branch must announce itself loudly — never a silent Program-projector skip"
    );
}

// ================================================================================================
// F. provisioning — DEFAULT-OFF is LOCKED: setup-imag.sh must never write (or even name) the
//    drm-output config; enabling it is a deliberate owner/supervisor runbook step
//    (.claude/rules/obs-drm-output.md), never provisioning
// ================================================================================================

#[test]
fn setup_imag_never_touches_the_drm_output_config_1152() {
    let body = read_repo("scripts/setup-imag.sh");
    assert!(
        !body.contains("drm-output.json"),
        "setup-imag.sh must NEVER write/enable ~/.camera-box/drm-output.json — the DRM output \
         ships DEFAULT-OFF and only the owner/supervisor flips it (the obs-drm-output rule's \
         runbook step)"
    );
}

// ================================================================================================
// G. verify-imag.sh — the acceptance gate proves the INSTALLED wrapper generation is
//    lease-tolerant (static content greps, enable-only: no start/restart)
// ================================================================================================

#[test]
fn verify_imag_gains_the_lease_tolerance_check_1152() {
    let body = read_repo("scripts/verify-imag.sh");
    assert!(
        body.contains("grep -c 'DRM_LEASE_MODE' /usr/local/bin/imag-obs-start.sh"),
        "verify-imag must grep the INSTALLED wrapper for the lease-mode marker"
    );
    assert!(
        body.contains("grep -c 'drm_output_lease_enabled' /usr/local/bin/imag_scenes.py"),
        "verify-imag must grep the INSTALLED seeder for the lease classifier"
    );
    // placed in the live flow after the (p) operator-scaffolding family, before the verdict
    let p_block = body
        .find("imag-obs-watchdog not in the agreed installed-but-disabled state")
        .expect("the (p) watchdog check must stay");
    let check = body
        .find("grep -c 'DRM_LEASE_MODE'")
        .expect("the lease-tolerance grep must exist");
    let clear = body.find("ALL CLEAR").expect("the verdict must stay");
    assert!(
        p_block < check && check < clear,
        "the lease-tolerance check belongs with the operator-scaffolding family, before the \
         verdict (p={p_block} check={check} clear={clear})"
    );
}

#[test]
fn verify_imag_lease_tolerance_helper_is_pure_and_two_tier_1152() {
    let verify = manifest_dir().join("scripts/verify-imag.sh");
    let run = |args: &str| -> i32 {
        let harness = format!(". \"$VERIFY\"\nimag_lease_tolerance_ok {args}\necho RC=$?");
        let out = Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .env("VERIFY", &verify)
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run bash harness");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("RC=").and_then(|v| v.parse().ok()))
            .unwrap_or_else(|| panic!("no RC= line in: {stdout:?}"))
    };
    assert_eq!(
        run("1 1"),
        0,
        "both markers present (1 hit each) = tolerant"
    );
    assert_eq!(run("3 2"), 0, "multiple hits are fine");
    assert_eq!(run("0 1"), 1, "wrapper without the marker = NOT tolerant");
    assert_eq!(
        run("1 0"),
        1,
        "seeder without the classifier = NOT tolerant"
    );
    assert_eq!(
        run("'' ''"),
        1,
        "unreadable (empty grep output) = hard FAIL, never a pass"
    );
    assert_eq!(run("x 1"), 1, "non-numeric garbage = hard FAIL");
}
