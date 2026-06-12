//! Regression guards for the remote kill/cleanup path on cam2 (#9 dispatch failure).
//!
//! Root cause chain found on the first real `workflow_dispatch` loopback run:
//!
//! 1. `multitap-e2e.sh`'s cleanup ran `pkill -f 'frame-probe'` INSIDE the inline ssh
//!    command string. `pkill -f` matches the full cmdline, and the remote shell's own
//!    cmdline contains the pattern text — so pkill killed the remote shell itself and the
//!    following `systemctl restart camera-box` NEVER ran. Every multitap run left cam2
//!    with a manual camera-box orphan and a stopped service.
//! 2. The loopback dispatch then hit that orphan still holding /dev/video0 → the manual
//!    camera-box start died with EBUSY (rc=3).
//! 3. The workflow's re-assert step had the same class of bug via a subtler route:
//!    `pkill -f '[c]amera-box'` doesn't match its own bracket pattern, but the SAME
//!    remote cmdline contained plain `camera-box` in the `systemctl` text → self-match
//!    again → ssh exit 255, repair step dead.
//!
//! The fix everywhere: `pkill -x <name>` (exact process-NAME match — a remote bash/sh
//! shell's comm never equals `camera-box`/`frame-probe`, so self-match is structurally
//! impossible), plus an orphan sweep + wait-until-/dev/video0-free in the loopback remote
//! script before starting the manual camera-box.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// No `pkill -f` anywhere in the rig scripts/workflow — only `pkill -x`. A `-f` pattern
/// executed through ssh can match the remote shell's own cmdline (directly, or via OTHER
/// text in the same command string), killing the cleanup mid-way. `-x` matches the
/// process name only and cannot self-match.
#[test]
fn rig_scripts_never_use_cmdline_matching_pkill() {
    for file in [
        "scripts/multitap-e2e.sh",
        "scripts/loopback-e2e.sh",
        ".github/workflows/loopback-e2e.yml",
    ] {
        let s = read(file);
        assert!(
            !s.contains("pkill -f"),
            "{file}: uses `pkill -f` — banned on the rig. The remote shell's own cmdline \
             can match the pattern (directly or via other text in the same command \
             string), so pkill kills the shell itself and the rest of the cleanup never \
             runs (this stranded a camera-box orphan on cam2 and broke the #9 dispatch). \
             Use `pkill -x <exact-process-name>` instead."
        );
        // Any pkill present must be the -x form.
        for line in s.lines().filter(|l| l.contains("pkill")) {
            assert!(
                line.contains("pkill -x"),
                "{file}: non `-x` pkill found: {line:?} — only exact-name `pkill -x` is \
                 allowed on the rig (immune to cmdline self-match)."
            );
        }
    }
}

/// The loopback remote script must sweep orphaned camera-box instances and WAIT until
/// /dev/video0 is actually free after `systemctl stop`, before starting the manual
/// camera-box. A fixed sleep races V4L2/uvcvideo teardown and any orphan → EBUSY (the
/// exact rc=3 failure of the first #9 dispatch).
#[test]
fn loopback_waits_for_video0_free_before_manual_start() {
    let s = read("scripts/loopback-e2e.sh");
    let stop = s
        .find("systemctl stop camera-box")
        .expect("loopback-e2e.sh: 'systemctl stop camera-box' not found");
    let start = s[stop..]
        .find("nohup /usr/local/bin/camera-box")
        .map(|i| stop + i)
        .expect("loopback-e2e.sh: manual camera-box start not found after the stop");
    let window = &s[stop..start];
    assert!(
        window.contains("pkill -x camera-box"),
        "loopback-e2e.sh: between `systemctl stop` and the manual start there must be an \
         orphan sweep (`pkill -x camera-box`) — a leftover manual instance from a broken \
         earlier cleanup otherwise holds /dev/video0 and the start dies with EBUSY."
    );
    assert!(
        window.contains("fuser") && window.contains("/dev/video0"),
        "loopback-e2e.sh: between `systemctl stop` and the manual start there must be a \
         wait-until-free loop on /dev/video0 (fuser), not a fixed sleep — V4L2/uvcvideo \
         teardown completes asynchronously after the process dies."
    );
}
