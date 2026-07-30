//! #734 — a manual `rig-mode.sh test` invocation shortly before the CI E2E gate makes the gate's
//! own `[3/8]`/#431 marker-growth check reliably FAIL (reproduced 2/2, 2026-07-13).
//!
//! ## The bug
//!
//! `[3/8]` (`scripts/recording-e2e.sh`) never killed a pre-existing `frame-probe` process before
//! waiting for `/dev/fb0` to free up and launching its OWN painter. If a manual `rig-mode.sh test`
//! invocation (or any stale leftover) left frame-probe running, it held BOTH `/dev/fb0` AND the
//! audio-marker's ALSA device (`hw:CARD=...,DEV=N`) EXCLUSIVELY:
//!
//! - the `fuser -s /dev/fb0` wait loop merely TIMES OUT after 15s (busy or not) with no failure
//!   branch, then `[3/8]` launched a SECOND frame-probe anyway;
//! - the #420 "RUNNING" self-check (`scripts/lib/audio-marker-check.sh`) reads the DEVICE-scoped
//!   `/proc/asound/.../status`, which reports `state: RUNNING` from the OLD still-alive process
//!   regardless of whether the run's OWN new process ever managed to open the device;
//! - the #431 emission-growth check polls THIS run's own `--marker-log` file, which the new
//!   process never gets to write if its `--audio-marker` open failed on the busy device.
//!
//! Net effect: `PASS: #420 ... RUNNING` immediately followed by `FAIL: #431 ... has NOT GROWN` —
//! exactly the live incident's shape, reproduced twice the same night.
//!
//! ## The fix
//!
//! `[3/8]` now unconditionally `pkill -x frame-probe`s BEFORE the fb0-wait loop, then polls
//! `pgrep -x frame-probe` until it reports nothing (bounded, ~10s) — so the gate run always starts
//! from a verified-clean state regardless of what was running before it (a manual test session, a
//! stale crashed leftover, or a previous run's own orphan). This deliberately does NOT try to
//! "reuse" a foreign running painter: it paints a DIFFERENT `--run-id` than the gate's own
//! `$RUN_ID`, and the verdict decode only trusts markers/QR burns carrying its OWN run id —
//! reusing a stale process's output would silently record the WRONG run's content.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Bound the search to the `[3/8]` cam2 painter-launch block ONLY (not the OTHER
/// `pkill -x frame-probe` occurrences elsewhere in the file — the AV_RESTART_GATE block, the
/// cleanup trap, the frozen-camera bounded retry — any of which would silently satisfy an
/// unbounded search even if `[3/8]` itself were never fixed).
fn step_3_of_8_block(s: &str) -> &str {
    let start = s
        .find("echo \"[3/8] cam2 (")
        .expect("#734: expected the [3/8] cam2 painter-launch step to exist");
    let end = s[start..]
        .find("PAINTER_LAUNCH_EPOCH=")
        .map(|i| start + i)
        .expect("#734: expected PAINTER_LAUNCH_EPOCH= to bound the [3/8] block");
    &s[start..end]
}

/// Locate the DEDICATED `_cam2_kill_existing` step (#734's own new variable) within the block —
/// NOT a bare substring search for "pkill -x frame-probe", which ALSO matches the PRE-EXISTING
/// `audio_marker_check_cmds`'s `on_fail_cmd` argument that already lived in this block before
/// #734 (a fire-and-forget cleanup for a DIFFERENT failure path, not a kill-before-launch
/// guarantee) — an unbounded substring match would give a false GREEN even on the pre-#734 script.
fn kill_existing_def_pos(block: &str) -> usize {
    block
        .find("_cam2_kill_existing=")
        .expect("#734: [3/8] must define a dedicated _cam2_kill_existing step")
}

#[test]
fn step_3_of_8_defines_a_kill_existing_step_that_pkills_and_verifies_dead() {
    let s = read("scripts/recording-e2e.sh");
    let block = step_3_of_8_block(&s);
    let def_pos = kill_existing_def_pos(block);
    // The definition itself (up to the next top-level statement) must both signal the kill AND
    // poll for the process actually being gone — a bare `pkill` with no verification is a RACE
    // (the old process may still hold the ALSA device for a moment after the signal is sent).
    let def_end = block[def_pos..]
        .find("\nsshpass")
        .map(|i| def_pos + i)
        .unwrap_or(block.len());
    let def = &block[def_pos..def_end];
    assert!(
        def.contains("pkill -x frame-probe"),
        "#734: _cam2_kill_existing must unconditionally kill any pre-existing frame-probe: {def}"
    );
    assert!(
        def.contains("pgrep -x frame-probe"),
        "#734: _cam2_kill_existing must VERIFY the kill actually took (poll pgrep), not just fire \
         pkill and hope: {def}"
    );
    let kill_pos = def.find("pkill -x frame-probe").unwrap();
    let verify_pos = def.find("pgrep -x frame-probe").unwrap();
    assert!(
        kill_pos < verify_pos,
        "#734: kill must come before the verify-dead poll within the step: {def}"
    );
}

#[test]
fn step_3_of_8_calls_kill_existing_before_the_fb0_wait_and_before_this_runs_own_launch() {
    let s = read("scripts/recording-e2e.sh");
    let block = step_3_of_8_block(&s);
    let def_pos = kill_existing_def_pos(block);

    // The definition alone isn't enough — it must actually be INVOKED (`$_cam2_kill_existing`)
    // inside the real ssh command, positioned BEFORE the fb0-fuser wait loop (a painter already
    // holding fb0 must be cleared first, not raced) and BEFORE this run's own painter launches.
    let call_pos = block[def_pos..]
        .find("$_cam2_kill_existing")
        .map(|i| def_pos + i)
        .expect(
            "#734: _cam2_kill_existing must be CALLED (\"$_cam2_kill_existing\") inside the ssh \
             command, not just defined and left unused",
        );
    let fb0_wait_pos = block[call_pos..]
        .find("while fuser -s /dev/fb0")
        .map(|i| call_pos + i)
        .expect("#734: expected the existing fb0-fuser wait loop to still be present");
    let launch_pos = block[fb0_wait_pos..]
        .find("nohup /tmp/frame-probe")
        .map(|i| fb0_wait_pos + i)
        .expect("#734: expected the harness's own painter launch to still be present");
    assert!(
        call_pos < fb0_wait_pos,
        "#734: kill-existing must be called BEFORE the fb0-wait loop: call_pos={call_pos} \
         fb0_wait_pos={fb0_wait_pos}"
    );
    assert!(
        fb0_wait_pos < launch_pos,
        "#734: the fb0-wait loop must still precede this run's own painter launch (unchanged \
         ordering): fb0_wait_pos={fb0_wait_pos} launch_pos={launch_pos}"
    );
}

#[test]
fn step_3_of_8_kill_existing_uses_pkill_dash_x_never_pkill_dash_f() {
    // This codebase's own established convention (rig-mode.sh, the cleanup trap, the AV_RESTART_GATE
    // block): `pkill -x` matches by process COMM name only, so it can NEVER self-match the remote
    // ssh session's own cmdline. A `pkill -f frame-probe` here would risk matching this very ssh
    // invocation's command line (which itself mentions "frame-probe" as a path/arg) and killing the
    // remote shell that's supposed to run the rest of the block.
    let s = read("scripts/recording-e2e.sh");
    let block = step_3_of_8_block(&s);
    assert!(
        !block.contains("pkill -f frame-probe"),
        "#734: must use `pkill -x frame-probe`, never `pkill -f frame-probe` (self-match risk): \
         {block}"
    );
}
