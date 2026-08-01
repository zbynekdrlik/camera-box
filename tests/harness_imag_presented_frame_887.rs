//! #887 -- imag's zero-loss proof stopped at OBS's own compositor (self-reported renderSkip/
//! activeFps); nothing verified what actually left imag-nb's HDMI-1 connector. User's settled
//! decision (2026-07-31, option 3, software-only): compare the compositor's OWN produced-frame
//! count (scripts/imag_produced_frame_check.py, see tests/python/test_imag_produced_frame_check.py)
//! against an INDEPENDENT observer -- the i915 kernel's per-CRTC CRC debugfs counter on the
//! connector actually driving HDMI-A-1 (scripts/lib/imag-presented-frame-check.sh).
//!
//! These tests (a) source the REAL lib and drive its pure connector->pipe parser against a
//! captured `i915_display_info` fixture -- no ssh, no rig -- proving it resolves the CRTC-scoped
//! match and is NOT confused by the later, unrelated "Connector info" summary section that
//! repeats every connector name with no CRTC context (the actual bug the first draft of this
//! parser hit live, 2026-08-01: a naive scan grabbed the LAST match, landing on pipe D from a
//! disabled CRTC instead of the real pipe B), and (b) pin the structural wiring into
//! scripts/recording-e2e.sh: report-only (never touches $GATE), honestly scoped field/comment
//! names (never claims "on the wall"/"at the projector").

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/imag-presented-frame-check.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn fixture_display_info() -> String {
    read("tests/fixtures/imag_i915_display_info_887.txt")
}

/// Source the lib, call `imag_presented_frame_pipe_for_connector CONNECTOR` with `text` fed on
/// stdin, return its stdout (the resolved pipe letter, or empty).
fn pipe_for_connector(text: &str, connector: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$SCRIPT\"; imag_presented_frame_pipe_for_connector \"$1\"")
        .arg("bash") // $0
        .arg(connector) // $1
        .env("SCRIPT", lib_script())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("run pipe_for_connector");
    assert!(out.status.success(), "pipe_for_connector must exit 0");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn resolves_hdmi_a_1_to_its_real_crtc_pipe_letter() {
    let got = pipe_for_connector(&fixture_display_info(), "HDMI-A-1");
    assert_eq!(
        got, "B",
        "#887: HDMI-A-1 is attached to [CRTC:268:pipe B] in the fixture -- a parser confused by \
         the later 'Connector info' summary section (which repeats HDMI-A-1 with no CRTC \
         context) would wrongly report the LAST-seen pipe letter (D, from a disabled CRTC) \
         instead. Got {got:?}"
    );
}

#[test]
fn resolves_edp_1_to_its_own_different_pipe_letter() {
    // A second connector in the SAME fixture, on a DIFFERENT pipe (A) -- proves the parser
    // tracks the CURRENT crtc block's pipe letter per-connector, not a single global value.
    let got = pipe_for_connector(&fixture_display_info(), "eDP-1");
    assert_eq!(
        got, "A",
        "#887: eDP-1 is attached to [CRTC:150:pipe A]. Got {got:?}"
    );
}

#[test]
fn unknown_connector_resolves_to_empty_never_a_stale_guess() {
    let got = pipe_for_connector(&fixture_display_info(), "DP-9");
    assert_eq!(
        got, "",
        "#887: a connector absent from the display -- never invent/guess a pipe letter for it"
    );
}

#[test]
fn empty_display_info_resolves_to_empty() {
    let got = pipe_for_connector("", "HDMI-A-1");
    assert_eq!(
        got, "",
        "#887: no display_info text at all -- must not crash or guess"
    );
}

// ---------------------------------------------------------------------------
// Structural wiring into scripts/recording-e2e.sh
// ---------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_presented_frame_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/imag-presented-frame-check.sh\""),
        "#887: recording-e2e.sh must source scripts/lib/imag-presented-frame-check.sh"
    );
}

#[test]
fn recording_e2e_runs_the_presented_frame_check_after_start_record() {
    let s = read("scripts/recording-e2e.sh");
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    let region_end = s[start_record..]
        .find("[6/8]")
        .map(|i| start_record + i)
        .expect("expected [6/8] to follow [5/8] StartRecord");
    let region = &s[start_record..region_end];
    assert!(
        region.contains("imag_presented_frame_resolve_cmd")
            && region.contains("imag_presented_frame_sample_cmds"),
        "#887: the presented-frame check must run between [5/8] StartRecord and [6/8] (during \
         the recording window it's meant to sample). Got region:\n{region}"
    );
    assert!(
        region.contains("imag_produced_frame_check.py"),
        "#887: the SAME region must also sample the compositor's own PRODUCED count, so the two \
         are compared over the same window."
    );
}

#[test]
fn presented_frame_check_never_touches_gate_report_only() {
    let s = read("scripts/recording-e2e.sh");
    let start = s
        .find("imag_presented_frame_resolve_cmd")
        .expect("expected the presented-frame resolve call to be wired in");
    let end = s[start..]
        .find("[6/8]")
        .map(|i| start + i)
        .expect("expected [6/8] to follow the presented-frame block");
    let region = &s[start..end];
    assert!(
        !region.contains("GATE=") && !region.contains("exit 1"),
        "#887: this is a REPORT-ONLY diagnostic (the weakest of the three options the ticket \
         names) -- it must never set $GATE or abort the run. Got region:\n{region}"
    );
}

#[test]
fn presented_frame_report_never_overstates_beyond_hdmi1() {
    // The honest-scoping requirement is part of the deliverable: grep the WHOLE lib + its
    // wiring in recording-e2e.sh for banned overreach phrasing.
    let lib = read("scripts/lib/imag-presented-frame-check.sh");
    for banned in [
        "on the wall",
        "on_wall",
        "at the projector",
        "at_projector",
        "on screen",
    ] {
        assert!(
            !lib.to_lowercase().contains(banned),
            "#887: the deliverable is honest scoping -- '{banned}' must never appear (a CRC-\
             observed frame proves it left the GPU on HDMI-1, nothing about the projector)"
        );
    }
}

#[test]
fn imag_produced_frame_check_reuses_obs_phase2_connection_helpers() {
    let s = read("scripts/imag_produced_frame_check.py");
    assert!(
        s.contains("from obs_phase2 import _conn, _rpc"),
        "#887: must reuse the proven obs-websocket connection/auth handshake (the same \
         import-reuse pattern as scripts/obs_burn_filter.py), never re-implement it"
    );
}
