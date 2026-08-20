//! #464 — behavioral tests for the presenter-aware painter-liveness check
//! (`scripts/lib/presenter-liveness-check.sh`).
//!
//! ## The bug (confirmed live on cam2, 2026-07-04)
//!
//! `scripts/rig-mode.sh` launches the QR painter with the default `--presenter auto` (no
//! `--presenter` flag in `painter_launch_remote`). On cam2's Intel i915 with a connected HDMI,
//! `auto` acquires DRM master and runs the KMS page-flip presenter
//! (`crate::probe::kms::KmsPresenter`, via `src/probe/presenter.rs::open_presenter`) — which BY
//! DESIGN never opens `/dev/fb0` (only the fbdev fallback, `crate::probe::fb::VsyncFb`, does; see
//! `src/presenter_kind.rs::resolve_presenter_kind`). The rig's liveness gate was a bare
//! `fuser -s /dev/fb0`, so a healthy, correctly-painting KMS run was reported
//! `FAIL: painter PID <pid> alive but NOT writing /dev/fb0` — a false failure.
//!
//! Live evidence reproduced exactly (PID 51673):
//!   - `presenter: using DRM/KMS page-flip (/dev/dri/card1)`
//!   - `KmsPresenter: 1920x1080@60.000Hz, double-buffered DRM page-flip, vblank-locked 1:1`
//!   - `fuser /dev/dri/card1` held by frame-probe; `fuser /dev/fb0` held by nobody
//!   - `/run/rig-qpsk-markers.csv` growing (genuinely painting)
//!
//! ## The fix (what these tests lock)
//!
//! `painter_liveness_check_cmds` (the new shared helper) reads the painter's own log
//! (`/tmp/rig-painter.log`, written by `open_presenter`'s own `tracing` lines) and asserts the
//! signal that actually matches the presenter in use: KMS -> the parsed DRM device is held
//! (`fuser -s <cardN>`) AND the `vblank-locked` confirmation line is present; anything else
//! (fbdev, or no KMS line at all) -> the original `fuser -s /dev/fb0` check, unchanged. FAIL LOUD
//! either way, `tail`ing the log so an operator sees which path fired.
//!
//! Same PURE-STRING-PLUS-EXECUTE model as `tests/harness_av_restart_audio_marker_check.rs`'s
//! #431 emission-check tests: the `fuser` device-liveness signal can't be faked with a real
//! `/proc` path (there's no real DRM/fb0 device in CI), so it is stubbed via a fake `fuser` on
//! PATH — the exact precedent `tests/harness_deploy_fleet.rs` already established for
//! `sshpass`/`systemctl`/`sha256sum`. Everything else (the log-fixture content, the generated
//! bash) is real; only the device-liveness syscall is faked. No ssh, no live rig.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const LIB_REL: &str = "scripts/lib/presenter-liveness-check.sh";

fn lib_path() -> PathBuf {
    manifest_dir().join(LIB_REL)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "presenter-liveness-464-{}-{}",
        std::process::id(),
        name
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn set_exec(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(p, perm).unwrap();
    }
}

/// Write a fake `fuser` executable to `bin_dir` that reports the device named in `held_device` as
/// held (exit 0) and every other device as NOT held (exit 1) — mirrors the live evidence exactly:
/// exactly ONE device is held at a time. `fuser` is invoked as `fuser -s <dev>` by the real
/// snippet, so the stub keys on `$2`.
fn stub_fuser(bin_dir: &Path, held_device: &str) {
    let p = bin_dir.join("fuser");
    fs::write(
        &p,
        format!(
            "#!/usr/bin/env bash\ncase \"$2\" in\n  {held_device}) exit 0 ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    set_exec(&p);
}

/// Write a fake `fuser` that reports NOTHING as held (every device fails) — the "painter died /
/// nothing is holding any device" case.
fn stub_fuser_nothing_held(bin_dir: &Path) {
    let p = bin_dir.join("fuser");
    fs::write(&p, "#!/usr/bin/env bash\nexit 1\n").unwrap();
    set_exec(&p);
}

/// Source the shared helper, build `painter_liveness_check_cmds LOG_FILE FB_DEVICE`, then `eval`
/// it in a SUBSHELL (so a FAIL branch's `exit 1` only ends the subshell, not this whole harness)
/// with `bin_dir` prepended to PATH so the stub `fuser` above is what actually runs. Returns
/// (exit_code, stdout, stderr).
fn run_check(log: &Path, fb_device: &str, bin_dir: &Path) -> (i32, String, String) {
    let lib = lib_path();
    let script = format!(
        // #1148: run the emitted snippet under `set -euo pipefail` — the SAME `set -e` the real
        // caller (rig-mode.sh painter_launch_remote) embeds it under. Under `-uo` alone a FAIL
        // token's non-zero `_reason=$(...)` assignment did NOT abort, so the granular FAIL-message
        // arms tested below would pass even though production silently skipped them (a silent
        // exit 1). With `-e`, those FAIL-message assertions genuinely guard that the `_cb_paint_
        // signal ... || true` keeps the case reachable.
        r#"set -euo pipefail
. "{lib}"
( eval "$(painter_liveness_check_cmds "{log}" "{fb_device}")" )
"#,
        lib = lib.display(),
        log = log.display(),
        fb_device = fb_device,
    );
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("PATH", &path_env)
        .output()
        .expect("#464: failed to run painter_liveness_check_cmds harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_log(dir: &Path, contents: &str) -> PathBuf {
    let p = dir.join("rig-painter.log");
    fs::write(&p, contents).unwrap();
    p
}

const KMS_LOG: &str = "presenter: using DRM/KMS page-flip (/dev/dri/card1)\n\
KmsPresenter: 1920x1080@60.000Hz, double-buffered DRM page-flip, vblank-locked 1:1\n\
painter: vblank-locked DRM page-flip \u{2014} tear-free 1:1 at 60Hz (--paint-fps ignored)\n";

const FALLBACK_LOG: &str = "presenter: DRM/KMS unavailable (open DRM device /dev/dri/card1: \
acquire DRM master (is another compositor holding the CRTC?)), falling back to fbdev (/dev/fb0)\n";

/// The shared helper must exist as its own source-only file (extracted, not left inline-only in
/// rig-mode.sh).
#[test]
fn shared_presenter_liveness_check_lib_exists() {
    assert!(
        lib_path().exists(),
        "#464: {} must exist — the presenter-aware liveness check must be extracted into a \
         shared helper",
        lib_path().display()
    );
}

/// #464 HEADLINE (the exact live reproduction): a KMS-painting run — the DRM device held, the
/// fbdev device held by NOBODY, the vblank-locked confirmation present — is exactly what the live
/// rig showed on cam2. The NEW presenter-aware check must PASS on it. The OLD check (a bare
/// `fuser -s /dev/fb0`) evaluated against the SAME stubbed reality would FAIL — reproducing the
/// #464 bug on the very same fixture the new check now handles correctly.
#[test]
fn kms_painter_healthy_run_passes_but_old_fb0_only_check_would_have_failed() {
    let dir = scratch("headline");
    let log = write_log(&dir, KMS_LOG);
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // Live evidence: /dev/dri/card1 held by frame-probe, /dev/fb0 held by nobody.
    stub_fuser(&bin, "/dev/dri/card1");

    let (code, out, err) = run_check(&log, "/dev/fb0", &bin);
    assert_eq!(
        code, 0,
        "#464: a healthy KMS-painting run must PASS the presenter-aware check. stdout={out:?} \
         stderr={err:?}"
    );
    assert!(
        out.contains("PASS") && out.contains("#464") && out.contains("/dev/dri/card1"),
        "#464: the PASS line must identify itself and the held DRM device. stdout:\n{out}"
    );

    // Reproduce the bug: the OLD check was exactly `fuser -s /dev/fb0` (unconditional, no
    // presenter awareness). Under the SAME stubbed reality (fb0 not held), that bare check fails
    // — proving this fixture is a genuine reproduction of the #464 false-FAIL, not a strawman.
    let old_check = format!(
        r#"export PATH="{}:$PATH"; fuser -s /dev/fb0 2>/dev/null"#,
        bin.display()
    );
    let old_status = Command::new("bash")
        .arg("-c")
        .arg(&old_check)
        .status()
        .expect("#464: failed to run the OLD fb0-only check for comparison");
    assert!(
        !old_status.success(),
        "#464: the OLD `fuser -s /dev/fb0`-only check must FAIL on this exact healthy-KMS \
         fixture — that IS the reproduced bug this PR fixes"
    );
}

/// A KMS log line present + the DRM device held, but NO `vblank-locked` confirmation anywhere in
/// the log, must FAIL LOUD — a degenerate/partial KMS state is not waved through just because the
/// device handle is open.
#[test]
fn kms_painter_without_vblank_lock_confirmation_fails_loud() {
    let dir = scratch("no-vblank");
    let log = write_log(
        &dir,
        "presenter: using DRM/KMS page-flip (/dev/dri/card1)\n",
    );
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub_fuser(&bin, "/dev/dri/card1");

    let (code, _out, err) = run_check(&log, "/dev/fb0", &bin);
    assert_ne!(
        code, 0,
        "#464: a KMS presenter with no vblank-locked confirmation must FAIL LOUD. stderr={err:?}"
    );
    assert!(
        err.contains("vblank-locked") && err.contains("#464"),
        "#464: the failure must explain the missing vblank-locked confirmation. stderr:\n{err}"
    );
}

/// A KMS log line present, but the parsed DRM device is NOT actually held (the painter process
/// died / never took DRM master despite the log line) must FAIL LOUD — the check is a genuine
/// liveness assertion, not a rubber stamp on the log text alone.
#[test]
fn kms_painter_with_device_not_held_fails_loud() {
    let dir = scratch("not-held");
    let log = write_log(&dir, KMS_LOG);
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub_fuser_nothing_held(&bin);

    let (code, out, err) = run_check(&log, "/dev/fb0", &bin);
    assert_ne!(
        code, 0,
        "#464: a KMS presenter whose DRM device is not actually held must FAIL LOUD. \
         stdout={out:?} stderr={err:?}"
    );
    assert!(
        err.contains("/dev/dri/card1") && err.contains("not held"),
        "#464: the failure must name the unheld DRM device. stderr:\n{err}"
    );
}

/// Backward compatibility (never weaken the gate): a log with NO KMS line (the fbdev fallback, or
/// an older painter build with no presenter log line at all) with `/dev/fb0` genuinely held must
/// still PASS via the original fb0 check.
#[test]
fn fbdev_fallback_with_fb0_held_passes() {
    let dir = scratch("fbdev-pass");
    let log = write_log(&dir, FALLBACK_LOG);
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub_fuser(&bin, "/dev/fb0");

    let (code, out, err) = run_check(&log, "/dev/fb0", &bin);
    assert_eq!(
        code, 0,
        "#464: the fbdev fallback path must still PASS when /dev/fb0 is genuinely held \
         (unchanged #68 behavior). stdout={out:?} stderr={err:?}"
    );
    assert!(
        out.contains("PASS") && out.contains("/dev/fb0"),
        "#464: expected an fbdev PASS line. stdout:\n{out}"
    );
}

/// Backward compatibility (never weaken the gate): the fbdev path must STILL fail loud when
/// `/dev/fb0` is genuinely not held — the original #247 bug-catching behavior must not be
/// loosened by this fix.
#[test]
fn fbdev_fallback_with_fb0_not_held_fails_loud() {
    let dir = scratch("fbdev-fail");
    let log = write_log(&dir, FALLBACK_LOG);
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub_fuser_nothing_held(&bin);

    let (code, out, err) = run_check(&log, "/dev/fb0", &bin);
    assert_ne!(
        code, 0,
        "#464: the fbdev path must still FAIL LOUD when /dev/fb0 is not held (no gate \
         weakening). stdout={out:?} stderr={err:?}"
    );
    assert!(
        err.contains("NOT writing /dev/fb0"),
        "#464: expected the original fbdev FAIL message to be preserved. stderr:\n{err}"
    );
}

/// A missing painter log (painter died before writing anything) must fall through to the fbdev
/// check (no KMS line found) and FAIL LOUD when fb0 is not held — never a false PASS on absent
/// evidence.
#[test]
fn missing_log_falls_back_to_fbdev_check_and_fails_loud_if_not_held() {
    let dir = scratch("missing-log");
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub_fuser_nothing_held(&bin);
    let missing = dir.join("does-not-exist.log");

    let (code, _out, _err) = run_check(&missing, "/dev/fb0", &bin);
    assert_ne!(
        code, 0,
        "#464: an absent painter log must not manufacture a false PASS"
    );
}

/// #464: on EITHER failure branch, the check must `tail` the painter log so an operator sees
/// which presenter path actually fired — the old FAIL branch (`alive but NOT writing /dev/fb0`)
/// did not even do this for the fbdev case.
#[test]
fn failure_tails_the_painter_log_for_diagnosis() {
    let dir = scratch("tail-on-fail");
    let log = write_log(
        &dir,
        "some earlier painter output\nUNIQUE_MARKER_LINE_464\n",
    );
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    stub_fuser_nothing_held(&bin);

    let (code, _out, err) = run_check(&log, "/dev/fb0", &bin);
    assert_ne!(code, 0, "#464: expected this fixture to fail");
    assert!(
        err.contains("UNIQUE_MARKER_LINE_464"),
        "#464: a FAIL branch must tail the painter log so an operator sees which path fired. \
         stderr:\n{err}"
    );
}

#[test]
fn lib_is_sourceable_without_side_effects() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(r#". "{}"; echo ok"#, lib_path().display()))
        .output()
        .expect("source lib");
    assert!(
        out.status.success(),
        "#464: sourcing the lib must be a no-op success"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
}

/// `rig-mode.sh` must source the shared helper (single source of truth, mirrors the #421
/// audio-marker-check.sh extraction).
#[test]
fn rig_mode_sources_the_shared_presenter_liveness_lib() {
    assert!(
        read("scripts/rig-mode.sh").contains("presenter-liveness-check.sh"),
        "#464: scripts/rig-mode.sh must source scripts/lib/presenter-liveness-check.sh"
    );
}

/// The headline reachability guard: `painter_launch_remote`'s step-5 liveness check must CALL the
/// shared `painter_liveness_check_cmds` (a command substitution, not just a prose mention), AFTER
/// the `kill -0` alive check and BEFORE the #420 audio-marker self-check — replacing the old bare
/// `fuser -s /dev/fb0`-only check as the SOLE "is it actually painting" signal.
#[test]
fn painter_launch_remote_calls_the_shared_liveness_check_between_alive_and_audio_checks() {
    let s = read("scripts/rig-mode.sh");
    let fn_start = s
        .find("painter_launch_remote() {")
        .expect("#464: expected painter_launch_remote() to exist");
    let alive_pos = s[fn_start..]
        .find("kill -0")
        .map(|i| i + fn_start)
        .expect("#464: expected the existing kill -0 alive check to still be present");
    let call_pos = s[fn_start..]
        .find("$(painter_liveness_check_cmds")
        .map(|i| i + fn_start)
        .expect(
            "#464: painter_launch_remote must CALL the shared painter_liveness_check_cmds \
             (command substitution), not just check /dev/fb0 inline",
        );
    let audio_check_pos = s[fn_start..]
        .find("$(audio_marker_check_cmds")
        .map(|i| i + fn_start)
        .expect("#464: expected the existing #420 audio-marker self-check to still be present");
    assert!(
        alive_pos < call_pos,
        "#464: the liveness check must run AFTER the kill -0 alive check"
    );
    assert!(
        call_pos < audio_check_pos,
        "#464: the liveness check must run BEFORE the #420 audio-marker self-check"
    );
}
