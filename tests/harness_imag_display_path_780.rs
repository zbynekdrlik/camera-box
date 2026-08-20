//! #780 — pure-function guard for `scripts/lib/imag-display-path.sh`, the SHARED display-path
//! drift gather + verdict core for the imag notebook (mirrors `scripts/lib/imag-power-envelope.sh`
//! #1040 and `scripts/lib/timesync-authority.sh` #596).
//!
//! Ticket root cause: the whole measurement chain (GetStats, the E2E recording verdict, static
//! screenshots) ends BEFORE the display path (OBS -> compositor -> GPU scanout -> HDMI), so a
//! projection lag/tearing that is really a CONFIG state lived in a layer with no test. #780 guards
//! those config states so a drift FAILs `drift-guard --check-imag` (and the E2E `[0/8]` preflight)
//! loudly, naming the facet.
//!
//! STEP-0 validation (live 10.77.9.182, read-only): the box is now Intel-iGPU-only (Raptor Lake-P
//! UHD, `modesetting`, no discrete NVIDIA) — so `nvidia-settings` is ABSENT and the ticket's
//! NVIDIA-era `GPUPowerMizerMode`/`ForceFullCompositionPipeline` facets do not apply as written
//! (#816/#841). The GENUINELY-applicable facets here: picom (a compositor breaks the tear-free
//! direct Present+PageFlip scanout #841 relies on), the Intel `imag-igpu-maxperf.service` freq-pin
//! (#841 — the real GPUPowerMizerMode=1 counterpart), and the #779 touchpad tap conf. FFCP is
//! obsolete-by-hardware (no NVIDIA -> no FFCP -> #790's +1-frame concern is inherently moot).
//!
//! Same convention as `tests/harness_imag_power_envelope_1040.rs`: source the REAL lib (it is
//! source-only, no side effects) and exercise the pure `imag_display_path_verdict` directly against
//! synthetic `|`-delimited gather fixtures — no rig, no ssh.
//!
//! RED before `scripts/lib/imag-display-path.sh` exists (sourcing fails, every test fails); GREEN
//! after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/imag-display-path.sh")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit_code, stdout).
fn run_sourced(body: &str) -> (i32, String) {
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

/// Run `imag_display_path_verdict "<gather>"` and return the stdout lines.
fn verdict(gather: &str) -> Vec<String> {
    let (rc, out) = run_sourced(&format!(
        "imag_display_path_verdict {}",
        shell_quote(gather)
    ));
    assert_eq!(rc, 0, "verdict must exit 0 (pure); stdout={out:?}");
    out.lines().map(str::to_string).collect()
}

/// The single `<facet>|<STATUS>|<detail>` line for `facet`, panicking if absent.
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

// ---- the five facets emit exactly one row each ------------------------------------------------

#[test]
fn verdict_emits_one_row_per_facet_780() {
    // A fully-clean Intel-box gather AFTER the issue-1146 REVERT: picom NOT running (the
    // compositor cost 21.57% render skips on the 25W envelope), picom.service disabled,
    // HDMI the xrandr primary, maxperf pinned+up, tap on.
    let g = "PICOM_PGREP|ok\nPICOM_PROC|\nPICOM_SERVICE|disabled\n\
             XRANDR|ok\nPRIMARY_OUTPUT|HDMI-1\n\
             MAXPERF_APPLICABLE|1\nMAXPERF_MIN|1400\nMAXPERF_RP0|1400\n\
             MAXPERF_ENABLED|enabled\nMAXPERF_ACTIVE|active\n\
             TAPCONF|present\nTAPCONF_TAPPING|on";
    let lines = verdict(g);
    for f in [
        "picom_process",
        "picom_service",
        "hdmi_primary",
        "igpu_maxperf",
        "tap_conf",
    ] {
        assert_eq!(
            status_of(&lines, f),
            "OK",
            "facet {f} on a clean gather: {lines:?}"
        );
    }
}

// ---- picom_process (issue 1146 REVERT: NOT running = OK — the #841 picom-off doctrine stands;
// the compositor tear fix cost 21.57% OBS render skips on the 25W envelope, live 2026-08-20) ----

#[test]
fn picom_process_ok_when_not_running_1146_revert() {
    // picom not running -> full render budget for OBS -> OK (issue 1146 revert).
    let lines = verdict("PICOM_PGREP|ok\nPICOM_PROC|");
    let l = facet_line(&lines, "picom_process");
    assert_eq!(status_of(&lines, "picom_process"), "OK", "{l}");
}

#[test]
fn picom_process_drift_when_running_1146_revert() {
    // picom running -> composites both outputs and starves the OBS render (21.57% skips
    // measured) -> DRIFT, naming the pid (issue 1146 revert).
    let lines = verdict("PICOM_PGREP|ok\nPICOM_PROC|2038724");
    let l = facet_line(&lines, "picom_process");
    assert!(l.contains("|DRIFT|"), "picom running must DRIFT: {l}");
    assert!(l.contains("2038724"), "the running pid must be named: {l}");
}

#[test]
fn picom_process_unknown_when_pgrep_missing_never_a_false_ok_780() {
    // #833: a missing tool must fail loud BY NAME, never read as a measured "not running = OK".
    let lines = verdict("PICOM_PGREP|missing");
    let l = facet_line(&lines, "picom_process");
    assert!(
        l.contains("|UNKNOWN|"),
        "missing pgrep must be UNKNOWN, not OK: {l}"
    );
    assert!(
        l.to_lowercase().contains("pgrep"),
        "must name the missing tool: {l}"
    );
}

#[test]
fn picom_process_unknown_when_not_gathered_780() {
    let lines = verdict("");
    assert_eq!(status_of(&lines, "picom_process"), "UNKNOWN");
}

// ---- picom_service (issue 1146 REVERT: the unit must stay DISABLED so the render-starving
// compositor never relaunches at login; package+config+unit stay installed dormant) ------------

#[test]
fn picom_service_ok_when_disabled_1146_revert() {
    let lines = verdict("PICOM_SERVICE|disabled");
    assert_eq!(status_of(&lines, "picom_service"), "OK");
}

#[test]
fn picom_service_drift_when_enabled_1146_revert() {
    // enabled -> the compositor comes back at every login and starves the render -> DRIFT.
    let lines = verdict("PICOM_SERVICE|enabled");
    assert_eq!(status_of(&lines, "picom_service"), "DRIFT");
}

#[test]
fn picom_service_unknown_when_not_gathered_1146() {
    let lines = verdict("");
    assert_eq!(status_of(&lines, "picom_service"), "UNKNOWN");
}

// ---- hdmi_primary (issue 1146: HDMI must be the xrandr primary — the projector is the vsync anchor)

#[test]
fn hdmi_primary_ok_when_hdmi_is_primary_1146() {
    let lines = verdict("XRANDR|ok\nPRIMARY_OUTPUT|HDMI-1");
    assert_eq!(status_of(&lines, "hdmi_primary"), "OK");
}

#[test]
fn hdmi_primary_drift_when_panel_is_primary_1146() {
    // the panel as primary makes IT the vsync anchor -> the projector tears (the exact #1146 beat).
    let lines = verdict("XRANDR|ok\nPRIMARY_OUTPUT|eDP-1");
    let l = facet_line(&lines, "hdmi_primary");
    assert!(l.contains("|DRIFT|"), "a non-HDMI primary must DRIFT: {l}");
    assert!(l.contains("eDP-1"), "the wrong primary must be named: {l}");
}

#[test]
fn hdmi_primary_unknown_when_xrandr_missing_never_a_false_verdict_1146() {
    // #833 discipline: a missing xrandr must be UNKNOWN by name, never a false OK/DRIFT.
    let lines = verdict("XRANDR|missing");
    let l = facet_line(&lines, "hdmi_primary");
    assert!(
        l.contains("|UNKNOWN|"),
        "missing xrandr must be UNKNOWN: {l}"
    );
    assert!(l.to_lowercase().contains("xrandr"), "must name xrandr: {l}");
}

#[test]
fn hdmi_primary_unknown_when_primary_unread_1146() {
    // xrandr present but no primary read (X unreachable over ssh, or no primary set) -> UNKNOWN,
    // never a false DRIFT.
    let lines = verdict("XRANDR|ok\nPRIMARY_OUTPUT|");
    assert_eq!(status_of(&lines, "hdmi_primary"), "UNKNOWN");
}

#[test]
fn hdmi_primary_unknown_when_not_gathered_1146() {
    let lines = verdict("");
    assert_eq!(status_of(&lines, "hdmi_primary"), "UNKNOWN");
}

// ---- igpu_maxperf (the Intel GPUPowerMizerMode=1 counterpart, #841) ----------------------------

#[test]
fn igpu_maxperf_ok_when_pinned_and_service_up_780() {
    let lines = verdict(
        "MAXPERF_APPLICABLE|1\nMAXPERF_MIN|1400\nMAXPERF_RP0|1400\n\
         MAXPERF_ENABLED|enabled\nMAXPERF_ACTIVE|active",
    );
    assert_eq!(status_of(&lines, "igpu_maxperf"), "OK");
}

#[test]
fn igpu_maxperf_drift_when_freq_floor_idles_below_ceiling_780() {
    let lines = verdict(
        "MAXPERF_APPLICABLE|1\nMAXPERF_MIN|100\nMAXPERF_RP0|1400\n\
         MAXPERF_ENABLED|enabled\nMAXPERF_ACTIVE|active",
    );
    let l = facet_line(&lines, "igpu_maxperf");
    assert!(l.contains("|DRIFT|"), "floor below ceiling must DRIFT: {l}");
}

#[test]
fn igpu_maxperf_drift_when_service_not_active_780() {
    let lines = verdict(
        "MAXPERF_APPLICABLE|1\nMAXPERF_MIN|1400\nMAXPERF_RP0|1400\n\
         MAXPERF_ENABLED|disabled\nMAXPERF_ACTIVE|inactive",
    );
    assert_eq!(status_of(&lines, "igpu_maxperf"), "DRIFT");
}

#[test]
fn igpu_maxperf_unknown_when_not_an_intel_igpu_box_780() {
    // hardware-agnostic (#816): no i915 gt sysfs -> UNKNOWN, never a false DRIFT.
    let lines = verdict("MAXPERF_APPLICABLE|0");
    assert_eq!(status_of(&lines, "igpu_maxperf"), "UNKNOWN");
}

#[test]
fn igpu_maxperf_unknown_when_not_gathered_780() {
    let lines = verdict("");
    assert_eq!(status_of(&lines, "igpu_maxperf"), "UNKNOWN");
}

// ---- tap_conf (#779) --------------------------------------------------------------------------

#[test]
fn tap_conf_ok_when_present_and_tapping_on_780() {
    let lines = verdict("TAPCONF|present\nTAPCONF_TAPPING|on");
    assert_eq!(status_of(&lines, "tap_conf"), "OK");
}

#[test]
fn tap_conf_drift_when_tapping_off_780() {
    let lines = verdict("TAPCONF|present\nTAPCONF_TAPPING|off");
    assert_eq!(status_of(&lines, "tap_conf"), "DRIFT");
}

#[test]
fn tap_conf_drift_when_conf_absent_780() {
    // the #779 conf being gone is a genuine drift (not a mere SSH hiccup — the block WAS gathered).
    let lines = verdict("TAPCONF|absent");
    assert_eq!(status_of(&lines, "tap_conf"), "DRIFT");
}

#[test]
fn tap_conf_unknown_when_not_gathered_780() {
    let lines = verdict("");
    assert_eq!(status_of(&lines, "tap_conf"), "UNKNOWN");
}

// ---- the remote gather snippet is a non-empty string the callers embed via $(...) --------------

#[test]
fn gather_remote_snippet_is_nonempty_and_names_the_sources_780() {
    let (rc, out) = run_sourced("imag_display_path_gather_remote_snippet");
    assert_eq!(rc, 0, "gather snippet must exit 0");
    assert!(
        out.contains("pgrep"),
        "snippet must probe picom via pgrep: {out}"
    );
    assert!(out.contains("picom"), "snippet must reference picom");
    assert!(
        out.contains("imag-igpu-maxperf.service"),
        "snippet must gather the #841 maxperf service"
    );
    assert!(
        out.contains("gt_min_freq_mhz") && out.contains("gt_RP0_freq_mhz"),
        "snippet must gather the iGPU freq floor + ceiling"
    );
    assert!(
        out.contains("30-touchpad-tap.conf"),
        "snippet must gather the #779 tap conf"
    );
    // #833 discipline: the snippet must emit a pgrep-presence marker, so a missing pgrep can be
    // told apart from a genuinely-idle picom.
    assert!(
        out.contains("PICOM_PGREP"),
        "snippet must emit a pgrep-presence marker"
    );
    // issue 1146: the snippet must gather the picom.service enable state (the *.target.wants
    // symlink) and emit a PICOM_SERVICE marker.
    assert!(
        out.contains("PICOM_SERVICE") && out.contains(".target.wants/picom.service"),
        "snippet must gather the picom.service enable symlink (issue 1146): {out}"
    );
    // issue 1146: the snippet must probe xrandr (#833) and gather the primary output.
    assert!(
        out.contains("xrandr") && out.contains("XRANDR") && out.contains("PRIMARY_OUTPUT"),
        "snippet must probe xrandr and gather the primary output (issue 1146): {out}"
    );
}

// ---- #780 item 6: the E2E [0/8] preflight is wired to the SAME shared lib -----------------------

fn read_repo(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn recording_e2e_sources_the_display_path_lib_780() {
    let body = read_repo("scripts/recording-e2e.sh");
    assert!(
        body.contains(r#". "$HERE/lib/imag-display-path.sh""#),
        "recording-e2e.sh must source the shared display-path lib"
    );
}

#[test]
fn recording_e2e_calls_the_preflight_fail_fast_before_dantesync_780() {
    let body = read_repo("scripts/recording-e2e.sh");
    // The [0/8] preflight call must exist, target the imag host, and hard-exit on a proven drift.
    let call = body
        .find("imag_display_path_preflight_assert \"$IMAG_IP\"")
        .expect("recording-e2e.sh must call imag_display_path_preflight_assert with the imag host");
    let win = &body[call..(call + 120).min(body.len())];
    assert!(
        win.contains("|| exit 1"),
        "the display-path preflight must fail-fast (|| exit 1): {win}"
    );
    // Early fail-fast: the display-path preflight banner must precede the DanteSync gate (mirrors
    // the #977 session-visibility ordering guard — don't burn the DanteSync/40-min path on a
    // known display-path config drift).
    let banner = body
        .find("imag display-path config preflight")
        .expect("a [0/8] banner announcing the display-path preflight must exist");
    let dantesync = body
        .find("[0/8] DanteSync NTP+PTP gate")
        .expect("the DanteSync banner must still exist");
    assert!(
        banner < dantesync,
        "the display-path preflight must run before the DanteSync gate (early fail-fast)"
    );
}
