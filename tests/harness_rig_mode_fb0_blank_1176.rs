//! issue 1176 — an EVENT-mode stop of cam2-painter must ALWAYS blank cam2's /dev/fb0.
//!
//! `painter_stop_remote` stops the painter with SIGTERM (kill $PID / pkill -x frame-probe). The
//! issue-660 clean blank runs only in KmsPresenter's Drop (a clean --duration-secs self-exit),
//! which SIGTERM bypasses -- so /dev/fb0 keeps the last painted frame (e.g. a lipsync ffmpeg
//! raw-fbdev write), and on cam2 (the #892 painter box, permanent NO_DISPLAY) the kernel fbdev
//! emulation then reveals that stale frame on the HDMI monitor once DRM master is released. The
//! shared `cam2_fb0_blank_cmds` builder, embedded in painter_stop_remote's #892 painter-box branch,
//! zeroes /dev/fb0 UNCONDITIONALLY (never gated on KILL_NEEDED like the ledger fallback).
//!
//! These tests source the REAL script and read its builders' RENDERED output (+ run the blank with a
//! fake `dd`) -- no rig.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/rig-mode.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn emit(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("run bash harness");
    assert!(
        out.status.success(),
        "builder harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write_fake(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    fs::write(&p, body).expect("write fake");
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&p, perm).unwrap();
}

#[test]
fn fb0_blank_builder_zeroes_the_framebuffer() {
    let b = emit("cam2_fb0_blank_cmds");
    assert!(
        b.contains("dd if=/dev/zero of=\"/dev/fb0\" bs=1M count=8 2>/dev/null || true"),
        "the builder must raw-zero /dev/fb0 (best-effort). Got:\n{b}"
    );
    assert!(b.contains("[#1176]"), "the blank must be logged. Got:\n{b}");
}

#[test]
fn painter_stop_blanks_fb0_in_the_painter_box_branch_in_order() {
    let p = emit("painter_stop_remote /run/rig-painter.pid");
    let dd = p
        .find("dd if=/dev/zero of=\"/dev/fb0\"")
        .expect("#1176: painter_stop_remote must blank /dev/fb0");
    // The #892 painter-box branch label must precede the blank (it lives inside that branch)...
    let branch = p
        .find("#868")
        .expect("#868/#892: the painter-box branch label must exist");
    // ...the painter must be confirmed STOPPED before the blank (fb0 released first)...
    let is_active = p
        .find("is-active cam2-painter")
        .expect("#892: the cam2-painter inactive check must exist");
    // ...and the blank must precede the else-branch's NO_DISPLAY assert (so branch_pos<nodisplay is kept).
    let interkom = p
        .find("interkom monitor not restored")
        .expect("#528: the non-painter-box interkom assert must still exist");
    assert!(
        branch < dd && is_active < dd && dd < interkom,
        "#1176: the fb0 blank must sit inside the #892 painter-box branch, after the painter is \
         confirmed stopped and before the else-branch interkom assert. \
         branch={branch} is_active={is_active} dd={dd} interkom={interkom}"
    );
}

#[test]
fn fb0_blank_line_has_no_self_matching_pkill() {
    // The #1176 blank must not introduce a full-cmdline pkill (the self-match footgun this rig avoids).
    let b = emit("cam2_fb0_blank_cmds");
    for line in b.lines() {
        assert!(
            !line.contains("pkill -f") && !line.contains("pgrep -f"),
            "#1176: the blank must never use full-cmdline matching. Got line: {line:?}"
        );
    }
}

#[test]
fn fb0_blank_actually_invokes_dd_on_fb0() {
    // Run the emitted blank with a fake `dd` on PATH and confirm it targets /dev/fb0.
    let b = emit("cam2_fb0_blank_cmds");
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("dd.log");
    write_fake(
        &bin,
        "dd",
        &format!(
            "#!/usr/bin/env bash\necho \"DD: $*\" >> \"{}\"\nexit 0\n",
            log.display()
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let status = Command::new("bash")
        .arg("-c")
        .arg(format!("set -e\n{b}"))
        .env("PATH", path)
        .status()
        .expect("run blank");
    assert!(
        status.success(),
        "the blank must be best-effort (never fail the caller)"
    );
    let logged = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logged.contains("of=/dev/fb0"),
        "#1176: the blank must invoke dd on /dev/fb0. dd calls:\n{logged}"
    );
}

#[test]
fn rig_mode_sources_the_fb0_blank_lib() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    assert!(
        s.contains("lib/cam2-fb0-blank.sh"),
        "rig-mode.sh must source the fb0-blank lib"
    );
}
