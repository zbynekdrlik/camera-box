//! #1148 — the SINGLE SOURCE OF TRUTH for the presenter-aware "cam2-painter is GENUINELY PAINTING
//! (not merely process-alive)?" signal now lives in `scripts/lib/cam2-paint-signal.sh` as the
//! remote-bash function `_cb_paint_signal` (emitted by `cam2_paint_signal_remote_fn`). It used to
//! be copy-pasted into FIVE builders (cam2_painter_restore_verify_cmds,
//! cam2_painter_steady_state_handoff_cmds, painter_liveness_check_cmds, mv_reverify_painter_up_cmds,
//! cam2_painter_genuine_paint_check_cmd), each re-tuning only the poll/exit shape AROUND the
//! identical predicate — a #863/#860 "never mask a black monitor" hazard, already drifting.
//!
//! THIS is where a future correction to the signal (an OBS presenter-log rename, a new presenter
//! backend) is tested. `_cb_paint_signal` reads the painter log on STDIN, takes an optional
//! FB_DEVICE arg, echoes ONE reason token, and RETURNS 0 iff genuinely painting:
//!   KMS_OK <dev> (0) | KMS_NODRM <dev> (1) | KMS_NOVBLANK <dev> (1) | FBDEV_OK (0) | FBDEV_DEAD (1)
//!
//! Driven with a fake `fuser` on PATH (no rig, no real DRM/fb device) exactly like
//! tests/harness_presenter_liveness_check.rs stubs its own `fuser`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn lib_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/cam2-paint-signal.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cam2-paint-signal-1148-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Write a fake `fuser` to `dir` that reports `held_device` (its `-s <dev>` 2nd arg) as HELD
/// (exit 0) and everything else as not held (exit 1). Pass "" to hold nothing.
fn stub_fuser(dir: &Path, held_device: &str) {
    let p = dir.join("fuser");
    fs::write(
        &p,
        format!("#!/usr/bin/env bash\n[ \"$2\" = \"{held_device}\" ] && exit 0 || exit 1\n"),
    )
    .unwrap();
    let mut perms = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&p, perms).unwrap();
}

/// Source the shared lib, define `_cb_paint_signal` via its builder, feed `log` on stdin, and run
/// it (optionally with `fb_device`). Returns (exit_code, reason_token_trimmed).
fn run_signal(log: &str, fb_device: Option<&str>, held_device: &str) -> (i32, String) {
    let dir = scratch("run");
    stub_fuser(&dir, held_device);
    let logf = dir.join("painter.log");
    fs::write(&logf, log).unwrap();
    let fb_arg = fb_device.map(|d| format!("\"{d}\"")).unwrap_or_default();
    let script = format!(
        r#"set -uo pipefail
export PATH="{bin}:$PATH"
. "{lib}"
eval "$(cam2_paint_signal_remote_fn)"
_cb_paint_signal {fb_arg} < "{log}"
"#,
        bin = dir.display(),
        lib = lib_path().display(),
        log = logf.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run _cb_paint_signal harness");
    let _ = fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

const KMS_VBLANK: &str = "presenter: using DRM/KMS page-flip (/dev/dri/card1)\n\
KmsPresenter: 1920x1080@60.000Hz, double-buffered DRM page-flip, vblank-locked 1:1\n";
const KMS_NO_VBLANK: &str = "presenter: using DRM/KMS page-flip (/dev/dri/card1)\n";
const NO_KMS: &str = "some unrelated painter log line\nno presenter selection here\n";

#[test]
fn lib_exists_and_defines_the_builder() {
    assert!(lib_path().exists(), "{} must exist", lib_path().display());
    let s = fs::read_to_string(lib_path()).unwrap();
    assert!(
        s.contains("cam2_paint_signal_remote_fn") && s.contains("_cb_paint_signal"),
        "#1148: the shared lib must define cam2_paint_signal_remote_fn -> _cb_paint_signal"
    );
    // Source-only: no `exit` in the emitted function (it must `return`, safe inside a set -e remote
    // AND inside cleanup()'s WARN-only EXIT trap).
    let start = s.find("cat <<'PAINTSIG'").expect("emitted heredoc body");
    let end = s[start..].find("\nPAINTSIG").map(|i| start + i).unwrap();
    assert!(
        !s[start..end].contains("exit "),
        "#1148: _cb_paint_signal must use `return`, never a bare `exit` (safe in cleanup()'s trap)"
    );
}

#[test]
fn sourcing_the_lib_is_side_effect_free() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(r#". "{}"; echo ok"#, lib_path().display()))
        .output()
        .expect("source lib");
    assert!(out.status.success(), "#1148: sourcing the lib must succeed");
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
}

#[test]
fn kms_page_flip_with_device_held_and_vblank_is_painting() {
    let (code, tok) = run_signal(KMS_VBLANK, None, "/dev/dri/card1");
    assert_eq!(
        code, 0,
        "#1148: a held KMS device + vblank-locked is genuinely painting. tok={tok}"
    );
    assert_eq!(
        tok, "KMS_OK /dev/dri/card1",
        "#1148: KMS_OK token must carry the parsed device"
    );
}

#[test]
fn kms_device_not_held_is_not_painting() {
    let (code, tok) = run_signal(KMS_VBLANK, None, "");
    assert_ne!(
        code, 0,
        "#1148: a KMS line whose DRM device is not held is NOT painting"
    );
    assert_eq!(tok, "KMS_NODRM /dev/dri/card1");
}

#[test]
fn kms_held_but_no_vblank_is_not_painting() {
    let (code, tok) = run_signal(KMS_NO_VBLANK, None, "/dev/dri/card1");
    assert_ne!(
        code, 0,
        "#1148: a held KMS device with no vblank-locked line is NOT painting"
    );
    assert_eq!(tok, "KMS_NOVBLANK /dev/dri/card1");
}

#[test]
fn fbdev_fallback_with_default_fb0_held_is_painting() {
    let (code, tok) = run_signal(NO_KMS, None, "/dev/fb0");
    assert_eq!(
        code, 0,
        "#1148: no KMS line + /dev/fb0 held is the fbdev painting path"
    );
    assert_eq!(tok, "FBDEV_OK");
}

#[test]
fn fbdev_fallback_with_nothing_held_is_not_painting() {
    let (code, tok) = run_signal(NO_KMS, None, "");
    assert_ne!(
        code, 0,
        "#1148: no KMS line + fb device not held is a dead (black) painter"
    );
    assert_eq!(tok, "FBDEV_DEAD");
}

#[test]
fn fbdev_device_is_configurable() {
    // A non-default fb device (e.g. /dev/fb1) is honored by the FB_DEVICE arg — the presenter-
    // liveness site passes it through. /dev/fb0 held but /dev/fb1 requested -> not painting.
    let (code_ok, tok_ok) = run_signal(NO_KMS, Some("/dev/fb1"), "/dev/fb1");
    assert_eq!(
        code_ok, 0,
        "#1148: FBDEV_OK on the requested /dev/fb1. tok={tok_ok}"
    );
    assert_eq!(tok_ok, "FBDEV_OK");
    let (code_dead, _t) = run_signal(NO_KMS, Some("/dev/fb1"), "/dev/fb0");
    assert_ne!(
        code_dead, 0,
        "#1148: the wrong fb device held must not read as painting"
    );
}

#[test]
fn empty_log_is_a_dead_fbdev_painter_not_a_false_pass() {
    let (code, tok) = run_signal("", None, "");
    assert_ne!(
        code, 0,
        "#1148: an empty painter log must never manufacture a false PASS"
    );
    assert_eq!(tok, "FBDEV_DEAD");
}
