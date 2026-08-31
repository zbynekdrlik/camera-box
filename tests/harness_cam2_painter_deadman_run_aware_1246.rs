//! #1246 — the cam2-painter dead-man must be RUN-AWARE: it must NEVER start `cam2-painter`
//! (whose `Wants=camera-box.service` pulls in production `camera-box`, whose ExecStartPre
//! `camera-box-free-capture-device.sh` stops every `camera-box-burn-*` unit) while a LIVE capture
//! burn owns `/dev/video`.
//!
//! ## The bug
//!
//! `scripts/lib/cam2-painter-deadman.sh`'s action guarded ONLY on `pgrep -x frame-probe`. Its
//! premise — "a live run always has a frame-probe owning fb0" — has a GAP: the harness stops the
//! deployed `cam2-painter` (its frame-probe exits) and starts the `camera-box-burn-cam2-<id>`
//! capture burn MANY seconds/minutes before it launches its own `[3/8]` `/tmp/frame-probe` painter.
//! A periodic deadman fire in that window sees no frame-probe, passes the guard, and runs
//! `systemctl start cam2-painter` → `Wants=camera-box.service` → its ExecStartPre frees the device
//! by stopping the LIVE burn. Live incident (cam2 journal, run 1635844760): burn Started
//! 19:01:02.787; deadman fired 19:01:49.544 (no-frame-probe gap); burn Stopped 19:01:49.624 —
//! invalidating an otherwise-green verdict via the `[7b/8]` run-integrity check.
//!
//! ## The fix
//!
//! Add a run-aware guard to the action: before `systemctl start cam2-painter`, no-op when a live
//! capture burn owns the device — `pgrep -x camera-box-burn` (the exact 15-char comm
//! free-capture-device.sh itself keys on) OR an active/activating `camera-box-burn-*` systemd unit
//! (blip-robust against the burn's `Restart=on-failure`). The burn owns the device across the WHOLE
//! measurement including the frame-probe gap, so this closes it. On-box + dev1-independent (the
//! #281 rig heartbeat is written on dev1 and is unreadable from this on-box timer).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const LIB: &str = "scripts/lib/cam2-painter-deadman.sh";

fn read_lib() -> String {
    let p = manifest_dir().join(LIB);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Slice the systemd-run action body (between `/bin/bash -c '` and its closing `' 2>/dev/null`) out
/// of the lib text — anchoring WITHIN the action so the lib's header COMMENT (which legitimately
/// discusses the guards) can never satisfy an ordering assertion vacuously.
fn action_body(lib: &str) -> String {
    const OPEN: &str = "/bin/bash -c '";
    let a0 = lib
        .find(OPEN)
        .expect("#1246: expected the systemd-run action")
        + OPEN.len();
    let rest = &lib[a0..];
    let end = rest
        .find("' 2>/dev/null")
        .expect("#1246: expected the action's closing quote");
    rest[..end].to_string()
}

// ---------------------------------------------------------------------------------------------- //
// Lib shape — the run-aware burn guard exists and precedes the painter start
// ---------------------------------------------------------------------------------------------- //

#[test]
fn action_guards_on_a_live_burn_before_starting_the_painter_1246() {
    let action = action_body(&read_lib());
    // The guard must reference the capture burn (the on-box "device claimed by a measurement"
    // signal) — either its process comm (pgrep -x camera-box-burn) or its systemd unit.
    let burn_guard = action.find("camera-box-burn").unwrap_or_else(|| {
        panic!(
            "#1246: the action must no-op when a live camera-box-burn owns the device, BEFORE \
             starting cam2-painter (which pulls camera-box and frees the device). Action:\n{action}"
        )
    });
    let start = action
        .find("systemctl start cam2-painter")
        .expect("#1246: expected the guarded painter start inside the action");
    assert!(
        burn_guard < start,
        "#1246: the burn guard ({burn_guard}) must precede the painter start ({start}) — otherwise \
         the start (and its device-freeing chain) runs before the run-aware check"
    );
    // The existing frame-probe guard must remain (fb0 owner), so BOTH device owners are covered.
    assert!(
        action.contains("pgrep -x frame-probe"),
        "#1246: the original frame-probe guard (fb0 painter owner) must remain alongside the new \
         burn guard (capture-device owner). Action:\n{action}"
    );
}

#[test]
fn burn_guard_uses_the_exact_15char_comm_not_bare_camera_box_1246() {
    // Must never match the 10-char PRODUCTION `camera-box` comm/unit — only the 15-char burn.
    let action = action_body(&read_lib());
    // The burn PROCESS guard must use the exact 15-char comm form (`-x camera-box-burn`),
    // mirroring free-capture-device.sh's own `pkill -9 -x camera-box-burn` — never a bare
    // `pgrep camera-box` that could match the 10-char production comm.
    assert!(
        action.contains("pgrep -x camera-box-burn"),
        "#1246: the burn process guard must be the exact-comm `pgrep -x camera-box-burn`. Action:\n{action}"
    );
    // Never a bare `pgrep -x camera-box` (would match production and disarm the deadman forever).
    assert!(
        !action.contains("pgrep -x camera-box\n")
            && !action.contains("pgrep -x camera-box ")
            && !action.contains("pgrep -x camera-box;")
            && !action.contains("pgrep -x camera-box'"),
        "#1246: the burn guard must never key on the 10-char production comm `camera-box` — only \
         the 15-char `camera-box-burn`. Action:\n{action}"
    );
}

// ---------------------------------------------------------------------------------------------- //
// Functional — re-exec the generated action under fake pgrep/systemctl (fake the remote, not ssh)
// ---------------------------------------------------------------------------------------------- //

/// Sources the real lib, generates the arm, extracts the `/bin/bash -c '...'` action, and runs it
/// under a PATH-restricted fake `pgrep`/`systemctl` (real `grep`). Returns the fake bins' call log.
/// `fp` = a frame-probe is running; `burn_proc` = `pgrep -x camera-box-burn` matches; `burn_unit`
/// = `systemctl list-units` reports an active camera-box-burn-* unit.
fn run_action_call_log(fp: bool, burn_proc: bool, burn_unit: bool) -> String {
    let script = r#"
set -uo pipefail
. "$SCRIPT"
FAKE="$(mktemp -d)"
LOG="$FAKE/calls.log"
cat > "$FAKE/pgrep" <<FAKEPG
#!/usr/bin/env bash
echo "pgrep \$*" >> "$LOG"
case "\$*" in
  "-x frame-probe")     [ "\$FP" = 1 ]        && exit 0 || exit 1 ;;
  "-x camera-box-burn") [ "\$BURN_PROC" = 1 ] && exit 0 || exit 1 ;;
esac
exit 1
FAKEPG
chmod +x "$FAKE/pgrep"
cat > "$FAKE/systemctl" <<FAKESC
#!/usr/bin/env bash
echo "systemctl \$*" >> "$LOG"
case "\$1 \$2" in
  "list-units --state=active,activating")
    # Model real systemd: columns are NAME LOAD ACTIVE SUB DESCRIPTION. A trailing unit-NAME
    # glob (camera-box-burn-*) filters by NAME only, NEVER the DESCRIPTION column. The deadman's
    # OWN transient unit is active while its action runs and (with no --description) its
    # description IS the command line, which contains "camera-box-burn" -- so a description-column
    # grep self-matches it (the #1246 red). The name-pattern form the fix uses cannot.
    pat=""
    for a in "\$@"; do case "\$a" in camera-box-burn-*) pat="\$a" ;; esac; done
    emit() { if [ -z "\$pat" ]; then echo "\$1 loaded active running \$2"; else case "\$1" in \$pat) echo "\$1 loaded active running \$2" ;; esac; fi; }
    emit "camera-box.service" "Camera Box - USB Video Capture to NDI"
    emit "cam2-painter-deadman.service" "/bin/bash -c pgrep -x camera-box-burn grep camera-box-burn"
    [ "\$BURN_UNIT" = 1 ] && emit "camera-box-burn-cam2-99.service" "/tmp/camera-box-burn-cam2-99"
    ;;
esac
exit 0
FAKESC
chmod +x "$FAKE/systemctl"
armtext="$(cam2_painter_deadman_arm_cmds)"
action="${armtext#*/bin/bash -c \'}"
action="${action%%\' 2>/dev/null*}"
PATH="$FAKE:/usr/bin:/bin" /usr/bin/env bash -c "$action"
cat "$LOG"
rm -rf "$FAKE"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("SCRIPT", manifest_dir().join(LIB))
        .env("FP", if fp { "1" } else { "0" })
        .env("BURN_PROC", if burn_proc { "1" } else { "0" })
        .env("BURN_UNIT", if burn_unit { "1" } else { "0" })
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "action harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn action_no_ops_when_a_burn_process_is_running_1246() {
    // The exact incident shape: no frame-probe (the gap), but a live burn PROCESS.
    let log = run_action_call_log(false, true, false);
    assert!(
        !log.contains("start cam2-painter"),
        "#1246: with a live camera-box-burn PROCESS and no frame-probe, the action must NOT start \
         cam2-painter (that would free the device and kill the live burn). Got:\n{log}"
    );
}

#[test]
fn action_no_ops_when_only_the_burn_unit_is_active_1246() {
    // Restart=on-failure blip: the process momentarily absent but the systemd unit active.
    let log = run_action_call_log(false, false, true);
    assert!(
        !log.contains("start cam2-painter"),
        "#1246: with an active camera-box-burn-* systemd unit (process momentarily absent during a \
         Restart blip) the action must still NOT start cam2-painter. Got:\n{log}"
    );
}

#[test]
fn action_still_no_ops_when_frame_probe_is_running_1246() {
    // The original #872 guard must still hold.
    let log = run_action_call_log(true, false, false);
    assert!(
        !log.contains("start cam2-painter"),
        "#1246: the original frame-probe guard must still no-op the start. Got:\n{log}"
    );
}

#[test]
fn action_starts_the_painter_when_no_frame_probe_and_no_burn_1246() {
    // A genuinely killed/idle run with the device free: the deadman must recover the painter.
    let log = run_action_call_log(false, false, false);
    assert!(
        log.contains("start cam2-painter"),
        "#1246: with no frame-probe AND no live burn (device free), the action MUST start \
         cam2-painter — the whole point of the dead-man. Got:\n{log}"
    );
}
