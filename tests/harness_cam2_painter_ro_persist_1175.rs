//! issue 1175 — changing cam2-painter.service's PERSISTENT enable-state must be safe on cam2's
//! READ-ONLY root. Both the EVENT `disable` (`cam2_painter_service_disable_cmds`) and the TEST
//! `enable --now` (the handoff, `cam2_painter_steady_state_handoff_cmds`) used
//! `systemctl <..> cam2-painter.service 2>/dev/null || true`, which SILENTLY swallowed the
//! `Read-only file system` failure of the /etc/systemd/system symlink write — so the unit's
//! persistent state never changed while the caller claimed it had (a disabled-but-still-enabled unit
//! re-arms the QR on a reboot; an enabled-but-not-enabled unit dies at the next reboot).
//!
//! The shared `cam2_painter_persist_state_cmds` builder opens a `mount -o remount,rw /` window, runs
//! the change FAIL-LOUD, restores ro, and VERIFIES the resulting `is-enabled` state. These tests
//! run its emitted remote bash with a fake `systemctl`/`mount`/`findmnt`/`fuser` on PATH — no rig.
//!
//! issue 1405: `enable-now` no longer runs `systemctl enable --now` inside the window. The ro
//! remount after it failed and the `|| true` hid it (cam2 ran on a read-write root, 4.10.2026; that
//! the start opened the blocking writer is inferred from the timing). Only `systemctl enable` runs in
//! the window; after the ro
//! remount `findmnt -no OPTIONS /` must read `ro` (else FAIL LOUD naming the writers, no start),
//! and only then `systemctl start`. The full run-the-text matrix lives in
//! `tests/python/test_cam2_painter_ro_verify_1405.py`.

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

/// Source rig-mode.sh (main skipped) and run `body`, returning stdout (asserts the builder exits 0).
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

/// Execute an emitted remote-bash snippet under `set -e` with a fake `systemctl` (is-enabled prints
/// `$ISENABLED`), `mount` (exits `$MOUNT_RC`), `findmnt` (the root reads `ro,relatime`) and `fuser`
/// on PATH. Returns (exit_code, combined output).
fn run_emitted(snippet: &str, is_enabled: &str, mount_rc: &str) -> (i32, String) {
    let (code, out, _calls) = run_emitted_on_root(snippet, is_enabled, mount_rc, "ro,relatime");
    (code, out)
}

/// As `run_emitted`, with the root mount options the fake `findmnt` reports after the window
/// (`ROOT_OPTS`). Also returns every `systemctl` call, one per line, so a test can prove the
/// painter was (or was never) started.
fn run_emitted_on_root(
    snippet: &str,
    is_enabled: &str,
    mount_rc: &str,
    root_opts: &str,
) -> (i32, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = dir.path().join("systemctl-calls");
    write_fake(
        &bin,
        "systemctl",
        "#!/usr/bin/env bash\necho \"$*\" >>\"$CALLS\"\ncase \"$1\" in\n  is-enabled) echo \"${ISENABLED:-disabled}\"; \
         [ \"${ISENABLED:-disabled}\" = enabled ] && exit 0 || exit 1 ;;\n  *) exit 0 ;;\nesac\n",
    );
    write_fake(
        &bin,
        "mount",
        "#!/usr/bin/env bash\nexit \"${MOUNT_RC:-0}\"\n",
    );
    write_fake(
        &bin,
        "findmnt",
        "#!/usr/bin/env bash\necho \"$ROOT_OPTS\"\n",
    );
    write_fake(
        &bin,
        "fuser",
        "#!/usr/bin/env bash\necho '/:                   root     76355 F.... systemd-journal' >&2\n",
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("set -e\n{snippet}"))
        .env("PATH", path)
        .env("ISENABLED", is_enabled)
        .env("MOUNT_RC", mount_rc)
        .env("ROOT_OPTS", root_opts)
        .env("CALLS", &calls)
        .output()
        .expect("run emitted");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    let called = fs::read_to_string(&calls).unwrap_or_default();
    (out.status.code().unwrap_or(-1), s, called)
}

// ---- the shared builder emits a remount-safe, fail-loud, read-back-verified change -------------- //

#[test]
fn disable_builder_is_remount_safe_fail_loud_and_read_back_verified() {
    let d = emit("cam2_painter_persist_state_cmds disable");
    assert!(
        d.contains("mount -o remount,rw /"),
        "must remount rw. Got:\n{d}"
    );
    assert!(
        d.contains("mount -o remount,ro /"),
        "must restore ro. Got:\n{d}"
    );
    assert!(
        d.contains("systemctl disable cam2-painter.service"),
        "must run the disable. Got:\n{d}"
    );
    assert!(
        d.contains("systemctl is-enabled cam2-painter.service"),
        "must read back is-enabled. Got:\n{d}"
    );
    assert!(d.contains("exit 1"), "must fail loud. Got:\n{d}");
    assert!(
        !d.contains("systemctl disable cam2-painter.service 2>/dev/null || true"),
        "must NOT swallow the disable failure any more. Got:\n{d}"
    );
}

#[test]
fn enable_builder_enables_in_the_window_and_starts_after_the_ro_verify_1405() {
    let e = emit("cam2_painter_persist_state_cmds enable-now");
    assert!(
        !e.contains("systemctl enable --now"),
        "issue 1405: the painter must never START inside the remount-rw window. Got:\n{e}"
    );
    let rw = e.find("mount -o remount,rw /").expect("must remount rw");
    let enable = e
        .find("systemctl enable cam2-painter.service ||")
        .expect("must enable inside the window (the call, not a message naming it)");
    let verify = e
        .find("findmnt -no OPTIONS /")
        .expect("issue 1405: must read the root mount state after the window");
    let start = e
        .find("systemctl start cam2-painter.service")
        .expect("issue 1405: must start the painter after the verify");
    assert!(
        rw < enable && enable < verify && verify < start,
        "issue 1405: remount rw -> enable -> ro verify -> start. Got:\n{e}"
    );
    assert!(
        e.contains("[ \"$_pss_state\" != \"enabled\" ]"),
        "must verify the unit ends up enabled. Got:\n{e}"
    );
}

// ---- run the emitted disable end-to-end against fake systemctl/mount --------------------------- //

#[test]
fn disable_happy_path_exits_zero_and_confirms_disabled() {
    let d = emit("cam2_painter_persist_state_cmds disable");
    let (code, out) = run_emitted(&d, "disabled", "0");
    assert_eq!(code, 0, "disable happy path must exit 0. out:\n{out}");
    assert!(
        out.contains("DISABLED + persisted"),
        "must confirm the disable persisted. out:\n{out}"
    );
}

#[test]
fn disable_on_read_only_root_fails_loud() {
    let d = emit("cam2_painter_persist_state_cmds disable");
    // mount -o remount,rw / fails (read-only root can't be made writable) -> FAIL LOUD.
    let (code, out) = run_emitted(&d, "disabled", "1");
    assert_ne!(code, 0, "a remount failure must fail loud. out:\n{out}");
    assert!(
        out.contains("could not remount"),
        "must name the remount failure. out:\n{out}"
    );
}

#[test]
fn disable_that_did_not_take_effect_fails_loud() {
    let d = emit("cam2_painter_persist_state_cmds disable");
    // remount ok but the unit is STILL enabled after disable -> the read-back must FAIL LOUD.
    let (code, out) = run_emitted(&d, "enabled", "0");
    assert_ne!(
        code, 0,
        "a disable that left the unit enabled must fail loud. out:\n{out}"
    );
    assert!(
        out.contains("still is-enabled"),
        "must name the still-enabled failure (a reboot would re-arm the QR). out:\n{out}"
    );
}

#[test]
fn enable_happy_path_exits_zero_and_confirms_enabled() {
    let e = emit("cam2_painter_persist_state_cmds enable-now");
    let (code, out, calls) = run_emitted_on_root(&e, "enabled", "0", "ro,relatime");
    assert_eq!(code, 0, "enable happy path must exit 0. out:\n{out}");
    assert!(
        out.contains("ENABLED + persisted"),
        "must confirm the enable persisted. out:\n{out}"
    );
    assert!(
        calls.lines().any(|l| l == "start cam2-painter.service"),
        "issue 1405: a verified read-only root must be followed by the start. calls:\n{calls}"
    );
}

#[test]
fn enable_with_the_root_left_rw_fails_loud_and_never_starts_1405() {
    let e = emit("cam2_painter_persist_state_cmds enable-now");
    let (code, out, calls) = run_emitted_on_root(&e, "enabled", "0", "rw,relatime");
    assert_ne!(
        code, 0,
        "issue 1405: a root left read-write must fail loud. out:\n{out}"
    );
    assert!(
        out.contains("FAIL: [#1405]") && out.contains("systemd-journal"),
        "issue 1405: the failure must name the writers holding / (fuser -vm /). out:\n{out}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("start") || l.contains("--now")),
        "issue 1405: the painter must NOT start on a writable root. calls:\n{calls}"
    );
}

#[test]
fn disable_with_the_root_left_rw_fails_loud_1405() {
    let d = emit("cam2_painter_persist_state_cmds disable");
    let (code, out, calls) = run_emitted_on_root(&d, "disabled", "0", "rw,relatime");
    assert_ne!(
        code, 0,
        "issue 1405: disable leaving a read-write root must fail loud. out:\n{out}"
    );
    assert!(
        out.contains("FAIL: [#1405]"),
        "must name the read-write root. out:\n{out}"
    );
    assert!(
        !calls.lines().any(|l| l.starts_with("start")),
        "disable must never start the painter. calls:\n{calls}"
    );
}

// ---- both call sites route through the shared builder ------------------------------------------ //

#[test]
fn disable_cmds_embeds_the_persist_builder_and_keeps_its_anchors() {
    let dc = emit("cam2_painter_service_disable_cmds");
    assert!(
        dc.contains("mount -o remount,rw /"),
        "disable_cmds must embed the remount. Got:\n{dc}"
    );
    assert!(
        dc.contains("systemctl stop cam2-painter.service"),
        "disable_cmds must keep its stop anchor. Got:\n{dc}"
    );
    assert!(
        dc.contains("#892"),
        "disable_cmds must keep its #892 label. Got:\n{dc}"
    );
    assert!(
        dc.contains("systemctl list-unit-files cam2-painter.service"),
        "disable_cmds must keep its list-unit-files guard. Got:\n{dc}"
    );
    assert!(
        !dc.contains("systemctl start cam2-painter.service")
            && !dc.contains("systemctl restart cam2-painter.service"),
        "disable_cmds must never (re)start the unit (#892). Got:\n{dc}"
    );
}

#[test]
fn handoff_embeds_the_persist_builder_for_the_enable() {
    let h = emit(
        "cam2_painter_steady_state_handoff_cmds /run/rig-painter.pid /run/rig-qpsk-markers.csv",
    );
    assert!(
        h.contains("systemctl enable cam2-painter.service")
            && !h.contains("systemctl enable --now"),
        "issue 1405: the handoff enables inside the window and never starts there. Got:\n{h}"
    );
    assert!(
        h.contains("mount -o remount,rw /"),
        "handoff's enable must go through the remount-rw window. Got:\n{h}"
    );
    assert!(
        h.contains("findmnt -no OPTIONS /") && h.contains("systemctl start cam2-painter.service"),
        "issue 1405: the handoff starts the painter only after the ro verify. Got:\n{h}"
    );
}

#[test]
fn rig_mode_sources_the_ro_persist_lib() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    assert!(
        s.contains("lib/cam2-painter-ro-persist.sh"),
        "rig-mode.sh must source the ro-persist lib"
    );
}
