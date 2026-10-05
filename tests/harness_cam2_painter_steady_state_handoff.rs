//! #1008 / #937 -- TEST mode's STEADY-STATE painter must be the PERMANENT supervised
//! cam2-painter.service (#863: Restart=always, WantedBy=multi-user.target), NOT a disposable 2h
//! `nohup frame-probe --duration-secs 7200` that dies silently and unsupervised.
//!
//! `do_test` keeps its transient painter only for the at-mode-set chain verification, then HANDS
//! steady state to the durable unit via
//! `scripts/lib/cam2-painter-handoff.sh::cam2_painter_steady_state_handoff_cmds()`: stop the
//! transient painter (free fb0/DRM, #440), `systemctl enable cam2-painter.service` inside a
//! remount-rw window (re-enable after any EVENT-mode #892 disable + survive reboot), verify the root
//! is read-only again, `systemctl start` it (issue 1405: never `enable --now` inside the window),
//! then FAIL LOUD unless it is active + genuinely painting (presenter-aware #464) + marker CSV
//! growing (#431).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/rig-mode.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source rig-mode.sh (BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The steady-state handoff remote bash (transient pidfile + the marker CSV the permanent unit writes).
fn handoff() -> String {
    run_sourced(
        "cam2_painter_steady_state_handoff_cmds /run/rig-painter.pid /run/rig-qpsk-markers.csv",
    )
}

fn script_text() -> String {
    std::fs::read_to_string(script()).expect("read rig-mode.sh")
}

#[test]
fn handoff_enables_and_starts_the_permanent_unit() {
    let h = handoff();
    let enable = h.find("systemctl enable cam2-painter.service ||").expect(
        "#1008: the handoff MUST enable the permanent cam2-painter.service -- so it survives \
         reboot AND re-arms after any EVENT-mode #892 `disable`",
    );
    let start = h.find("systemctl start cam2-painter.service").expect(
        "#1008: the handoff MUST start the permanent unit as the durable steady-state painter",
    );
    assert!(
        enable < start && !h.contains("systemctl enable --now"),
        "issue 1405: enable inside the remount-rw window, start only after it (cam2's root stayed \
         read-write after an enable --now inside the window). Got:\n{h}"
    );
}

/// issue 1405: `rig-mode.sh test` arms the cam2-painter dead-man after a good handoff, and only
/// EVENT disarms it. A second TEST whose handoff fails at the ro verify would otherwise leave the
/// first TEST's dead-man live, and it starts the painter on the writable root within ~5 min. So the
/// handoff disarms it before it stops the transient painter (do_test re-arms it after a good
/// handoff).
#[test]
fn handoff_disarms_the_deadman_before_the_window_1405() {
    let h = handoff();
    let disarm = h
        .find("systemctl stop cam2-painter-deadman.timer")
        .expect("issue 1405: the handoff must disarm the cam2-painter dead-man");
    let stop_transient = h
        .find("cat \"/run/rig-painter.pid\"")
        .expect("#440: the transient painter stop");
    let enable = h
        .find("systemctl enable cam2-painter.service ||")
        .expect("the enable call");
    assert!(
        disarm < stop_transient && stop_transient < enable,
        "issue 1405: disarm the dead-man, then stop the transient painter, then the window. Got:\n{h}"
    );
}

#[test]
fn handoff_stops_transient_painter_before_enabling_permanent_unit() {
    let h = handoff();
    let stop_pos = h.find("cat \"/run/rig-painter.pid\"").expect(
        "#440: handoff must stop the TRANSIENT painter via its pidfile before starting the unit",
    );
    let enable_pos = h
        .find("systemctl enable cam2-painter.service ||")
        .expect("#1008: handoff must enable+start the permanent unit");
    assert!(
        stop_pos < enable_pos,
        "#440: the transient verification painter must be stopped (fb0/DRM freed) BEFORE the \
         permanent unit is started, so the two never race /dev/fb0. Got:\n{h}"
    );
}

#[test]
fn handoff_stops_transient_via_pidfile_never_pkill_frame_probe() {
    let h = handoff();
    assert!(
        !h.contains("pkill -f frame-probe") && !h.contains("pkill -x frame-probe"),
        "the handoff must stop the transient painter via its PID FILE only -- a `pkill ... \
         frame-probe` would also match the remote shell's own cmdline (the self-kill footgun this \
         rig avoids) AND could kill the permanent unit's frame-probe. Got:\n{h}"
    );
}

#[test]
fn handoff_fails_loud_when_permanent_unit_not_installed() {
    let h = handoff();
    assert!(
        h.contains("systemctl list-unit-files cam2-painter.service"),
        "#1008: the handoff must CHECK the unit is installed. Got:\n{h}"
    );
    assert!(
        h.contains("exit 1"),
        "#1008: a missing cam2-painter.service means TEST mode cannot hand steady-state to a \
         durable supervised painter -- FAIL LOUD (exit 1), never silently leave a 2h nohup. Got:\n{h}"
    );
}

#[test]
fn handoff_verifies_unit_active_and_painting_fail_loud() {
    let h = handoff();
    assert!(
        h.contains("systemctl is-active cam2-painter.service"),
        "#1008: the handoff must verify the unit actually became active. Got:\n{h}"
    );
    // Presenter-aware (#464): KMS page-flip holds a DRM card, never /dev/fb0; fbdev fallback holds fb0.
    assert!(
        h.contains("presenter: using DRM/KMS page-flip") && h.contains("/dev/fb0"),
        "#464: the painting check must be presenter-aware (KMS DRM device held+vblank-locked, or \
         /dev/fb0 held on the fbdev path). Got:\n{h}"
    );
    assert!(
        h.matches("exit 1").count() >= 2,
        "#1008: both the not-active and the not-painting branches must FAIL LOUD (exit 1). Got:\n{h}"
    );
}

#[test]
fn handoff_verifies_marker_csv_is_growing() {
    let h = handoff();
    // Reuses audio_marker_emission_check_cmds (#431) against the permanent unit's own marker CSV --
    // "must stay alive" means the QPSK marker log keeps growing, not merely that a process is up.
    assert!(
        h.contains("/run/rig-qpsk-markers.csv") && h.contains("has NOT GROWN"),
        "#431/#1008: the handoff must assert the marker CSV the permanent unit writes keeps \
         GROWING (audio_marker_emission_check_cmds), not just that the process is alive. Got:\n{h}"
    );
}

#[test]
fn do_test_sources_and_calls_the_steady_state_handoff() {
    let s = script_text();
    assert!(
        s.contains("lib/cam2-painter-handoff.sh"),
        "#1008: rig-mode.sh must source the handoff lib. Got script without it."
    );
    assert!(
        s.contains("cam2_painter_steady_state_handoff_cmds"),
        "#1008: do_test must CALL cam2_painter_steady_state_handoff_cmds to hand steady state to \
         the durable unit -- otherwise TEST mode still leaves a disposable nohup as steady state."
    );
}
