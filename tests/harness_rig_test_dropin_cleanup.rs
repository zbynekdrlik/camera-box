//! #309 — the rig-mode #291 no-display drop-in must NOT leak into the sibling e2e harness cleanups.
//!
//! ## The bug
//!
//! #291's `scripts/rig-mode.sh test` installs a TRANSIENT systemd drop-in
//! (`/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf`) that overrides ExecStart
//! to run camera-box WITHOUT `--display` (frees /dev/fb0 for the QR painter). The sibling harnesses
//! restore camera-box in their own cleanup with NO knowledge of that drop-in:
//!   - `scripts/recording-e2e.sh` cleanup runs `systemctl restart camera-box`
//!   - `scripts/loopback-e2e.sh`  cleanup runs `systemctl start camera-box`
//! If an operator ran `rig-mode.sh test` and then one of these harnesses standalone, the harness
//! "restore" brings camera-box back in NO-DISPLAY mode — the interkom return monitor stays dark while
//! the operator believes broadcast is fully restored (the #246-class silent test-state leak).
//!
//! ## The fix (what these tests lock)
//!
//! A single-sourced helper `scripts/lib/rig-test-dropin.sh` owns the drop-in PATH and a pure
//! `rig_test_dropin_clear_cmds` builder that emits the REMOTE bash to remove the drop-in (+ its now
//! empty dir) and `daemon-reload`. `rig-mode.sh`, `recording-e2e.sh` and `loopback-e2e.sh` all source
//! it, so the path lives in exactly ONE place and EVERY camera-box restore clears the drop-in first.
//! Same PURE-string model as tests/rig_mode.rs / tests/harness_recording_e2e_paths.rs: source the
//! real helper, run/inspect its output — NO ssh to a live rig.

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

const DROPIN_BASENAME: &str = "zz-rig-test-no-display.conf";
const LIB_REL: &str = "scripts/lib/rig-test-dropin.sh";

/// Headline behavioral RED→GREEN: the shared clear builder must emit remote bash that REMOVES a
/// rig-test no-display drop-in. We create a fake drop-in at a temp path, run the emitted commands
/// locally, and assert the file is GONE afterwards. This is exactly what the harness restore now
/// runs over ssh against the cam box — proven here without a live rig.
#[test]
fn shared_clear_removes_the_dropin() {
    let lib = manifest_dir().join(LIB_REL);
    assert!(
        lib.exists(),
        "#309: {} must exist (single-sourced rig-test drop-in helper)",
        lib.display()
    );

    // A throwaway drop-in tree under target/ (CI-writable), mirroring the real /run path shape.
    let base = manifest_dir().join(format!("target/it-rig-dropin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let dropin_dir = base.join("camera-box.service.d");
    fs::create_dir_all(&dropin_dir).expect("mk fake drop-in dir");
    let dropin = dropin_dir.join(DROPIN_BASENAME);
    fs::write(&dropin, "[Service]\nExecStart=\nExecStart=/usr/local/bin/camera-box\n")
        .expect("write fake drop-in");
    assert!(dropin.exists(), "precondition: fake drop-in created");

    // Source the helper, emit the clear commands for our fake path, and RUN them locally.
    // `systemctl daemon-reload` is absent/fails on CI, but the builder emits `|| true`, so the
    // rm/rmdir still execute and the drop-in is removed.
    let harness = "set -uo pipefail\n. \"$LIB\"\neval \"$(rig_test_dropin_clear_cmds \"$DROPIN\")\"\n";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", &lib)
        .env("DROPIN", &dropin)
        .output()
        .expect("run clear harness");
    assert!(
        out.status.success(),
        "#309: the clear builder must run cleanly (|| true tolerates a non-systemd CI host).\n\
         stdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let gone = !dropin.exists();
    let _ = fs::remove_dir_all(&base);
    assert!(
        gone,
        "#309: the shared clear must REMOVE the rig-test no-display drop-in (it is still present)"
    );
}

/// Idempotent: running the clear when NO drop-in is present must succeed (a clean no-op) — so wiring
/// it into every restore can never break a normal restart. (The issue's explicit requirement:
/// "Keep behavior identical when no drop-in is present — rm -f is a no-op; daemon-reload is safe".)
#[test]
fn shared_clear_is_noop_when_absent() {
    let lib = manifest_dir().join(LIB_REL);
    assert!(lib.exists(), "#309: {} must exist", lib.display());
    let absent = manifest_dir().join(format!(
        "target/it-rig-dropin-absent-{}/camera-box.service.d/{DROPIN_BASENAME}",
        std::process::id()
    ));
    let harness = "set -uo pipefail\n. \"$LIB\"\neval \"$(rig_test_dropin_clear_cmds \"$DROPIN\")\"\n";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", &lib)
        .env("DROPIN", &absent)
        .output()
        .expect("run clear harness");
    assert!(
        out.status.success(),
        "#309: clearing an absent drop-in must be a clean no-op; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// DRY / single-source guard: the literal drop-in path must live in EXACTLY one place — the shared
/// helper. After the fix neither rig-mode.sh nor the two sibling harnesses hard-codes it (they all
/// source the helper), so the path can never desync across the scripts ("do NOT duplicate the path
/// string in three files").
#[test]
fn dropin_path_is_single_sourced_in_the_lib() {
    assert!(
        read(LIB_REL).contains(DROPIN_BASENAME),
        "#309: the shared helper must own the drop-in path (single source of truth)"
    );
    for s in [
        "scripts/rig-mode.sh",
        "scripts/recording-e2e.sh",
        "scripts/loopback-e2e.sh",
    ] {
        assert!(
            !read(s).contains(DROPIN_BASENAME),
            "#309: {s} must NOT hard-code the drop-in path — source it from {LIB_REL} (no duplication)"
        );
    }
}

/// rig-mode.sh must source the shared helper (so its install/remove and the harnesses share one path).
#[test]
fn rig_mode_sources_the_shared_helper() {
    assert!(
        read("scripts/rig-mode.sh").contains("rig-test-dropin.sh"),
        "#309: rig-mode.sh must source scripts/lib/rig-test-dropin.sh (single-sourced drop-in path)"
    );
}

/// recording-e2e.sh must source the helper and CLEAR the drop-in in its cam2 camera-box restore —
/// BEFORE `systemctl restart camera-box` — so a leftover `rig-mode.sh test` drop-in can't bring
/// camera-box back in no-display mode (dark interkom monitor).
#[test]
fn recording_e2e_clears_dropin_before_restart() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("rig-test-dropin.sh"),
        "#309: recording-e2e.sh must source the shared rig-test-dropin helper"
    );
    let clear = s
        .find("rig_test_dropin_clear_cmds")
        .expect("#309: recording-e2e.sh must call rig_test_dropin_clear_cmds in its cam2 restore");
    let restart = s[clear..]
        .find("systemctl restart camera-box")
        .map(|i| clear + i)
        .expect("#309: recording-e2e.sh cam2 restore must restart camera-box after the clear");
    assert!(
        clear < restart,
        "#309: the drop-in clear must come BEFORE the cam2 `systemctl restart camera-box`"
    );
}

/// loopback-e2e.sh must source the helper, carry the clear commands into the remote shell, and RUN
/// them in its remote cleanup BEFORE `systemctl start camera-box`.
#[test]
fn loopback_e2e_clears_dropin_before_start() {
    let s = read("scripts/loopback-e2e.sh");
    assert!(
        s.contains("rig-test-dropin.sh"),
        "#309: loopback-e2e.sh must source the shared rig-test-dropin helper"
    );
    assert!(
        s.contains("rig_test_dropin_clear_cmds"),
        "#309: loopback-e2e.sh must build the clear commands from the shared helper"
    );
    let eval = s
        .find("eval \"${RIG_TEST_DROPIN_CLEAR")
        .or_else(|| s.find("eval \"$RIG_TEST_DROPIN_CLEAR"))
        .expect("#309: loopback-e2e.sh remote cleanup must eval the passed-in clear commands");
    let start = s[eval..]
        .find("systemctl start camera-box")
        .map(|i| eval + i)
        .expect("#309: loopback-e2e.sh remote cleanup must start camera-box after the clear");
    assert!(
        eval < start,
        "#309: the drop-in clear must come BEFORE the remote `systemctl start camera-box`"
    );
}
