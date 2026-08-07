//! Patch-presence guard for #78 (the "B′" patch) — DistroAV's NDI OUTPUT stamps the
//! real DanteSync wall-clock emit timecode, NOT `NDIlib_send_timecode_synthesize`.
//!
//! Background: stock DistroAV stamps the outgoing NDI frame timecode with
//! `NDIlib_send_timecode_synthesize` — a counter the NDI SDK seeds ONCE from system
//! start and then free-runs, baking the start-time pipeline buffering into a FIXED
//! ~150 ms lag between the emit timecode and the real frame instant (#76). The #78
//! genlock fork replaces that in `vendor/distroav/src/ndi-output.cpp` with the OS
//! wall clock (DanteSync-disciplined, every node sub-ms-locked), snapped to the
//! 1/fps frame boundary AT OR BEFORE the emit instant (#1009: FLOOR — the pre-1009
//! ceil/strictly-next boundary dated every frame 0..1 interval into the receiver's
//! future and armed the issue-147 backward-step hair-trigger, the 2026-08-07
//! overnight −900 ms hold collapse) in 100 ns units since the Unix epoch — the SAME
//! per-second grid and boundary math as the camera-box sender (`src/ndi.rs`
//! `floor_boundary_100ns`), so cam→OBS and OBS↔OBS latency share one timebase. That
//! is what the QR latency instrument depends on.
//!
//! This is a SOURCE-level guard, not a runtime test (same convention as
//! tests/genlock_preload.rs and tests/obs_updater_disabled.rs): the genlock patch
//! lives in the vendored C++ (`git log -- vendor/` is the patch series, per
//! vendor/README.md). The risk it defends against is a future `git subtree pull`
//! upstream release-bump (the `/update-av-stack` flow, #44) silently restoring the
//! `synthesize` assignment in the output path and reintroducing the ~150 ms timecode
//! lag — which `scripts/drift-guard.sh` would NOT catch (it pins the DistroAV
//! VERSION 6.2.1, not fork-patch CONTENT, so a non-timecode-patched 6.2.1 dll passes
//! undetected). If the patch reverts, CI fails loudly HERE — the "report conflicts
//! loudly" contract of the monorepo (#85).
//!
//! Note on the bare token: `NDIlib_send_timecode_synthesize` legitimately still
//! appears (a) in the explanatory comment at the top of ndi-output.cpp describing
//! what the patch replaced, and (b) in ndi-filter.cpp (a DIFFERENT, non-output path
//! that is intentionally NOT patched). So the regression assertion below targets the
//! ACTUAL output-frame ASSIGNMENT (`video_frame.timecode = NDIlib_send_timecode_synthesize`),
//! never the bare token — a substring grep would false-fail on the comment.

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";

#[test]
fn output_video_timecode_uses_real_wall_clock_not_synthesize() {
    let src = squish(&vendor_file(NDI_OUTPUT));

    // The wall-clock helpers the #78 patch introduces MUST be present.
    assert!(
        src.contains("genlock_wall_now_100ns"),
        "{NDI_OUTPUT}: #78 patch missing — `genlock_wall_now_100ns` (the real \
         DanteSync wall-clock source) is gone. A `git subtree pull` (#44) likely \
         reverted the timecode patch; re-apply it."
    );
    assert!(
        src.contains("genlock_emit_timecode_100ns"),
        "{NDI_OUTPUT}: #78 patch missing — `genlock_emit_timecode_100ns` (the \
         boundary-aligned emit timecode) is gone. A `git subtree pull` (#44) likely \
         reverted the timecode patch; re-apply it."
    );

    // The OUTPUT frame must be stamped with the real wall-clock helper at the emit path.
    assert!(
        src.contains("video_frame.timecode = genlock_emit_timecode_100ns("),
        "{NDI_OUTPUT}: #78 — the video output frame no longer stamps \
         `genlock_emit_timecode_100ns` on its timecode. The real-wall-clock emit \
         patch was reverted; re-apply it."
    );
    assert!(
        src.contains("audio_frame.timecode = genlock_wall_now_100ns()"),
        "{NDI_OUTPUT}: #78 — the audio output frame no longer stamps \
         `genlock_wall_now_100ns` on its timecode. The real-wall-clock emit patch was \
         reverted; re-apply it."
    );

    // #1009: the emit stamp must be the FLOOR boundary (at-or-before the emit instant) —
    // both the floor helper's presence and the ceil helper's ABSENCE are pinned, so a
    // subtree pull restoring the old helper silently re-biasing the stamps fails HERE.
    assert!(
        src.contains("return genlock_floor_boundary_100ns(genlock_wall_now_100ns(), fps);"),
        "{NDI_OUTPUT}: #1009 — the emit timecode no longer stamps the FLOOR boundary          (genlock_floor_boundary_100ns); the sender future-bias that armed the issue-1007          backward-step hair-trigger is back. Re-apply the floor-stamp patch."
    );
    assert!(
        !src.contains("genlock_next_boundary_100ns"),
        "{NDI_OUTPUT}: #1009 — the pre-1009 ceil boundary helper name is back in the          output path; emit stamps may be future-dated again. Keep the floor helper          (mirror: src/ndi.rs floor_boundary_100ns)."
    );

    // And the stock `synthesize` ASSIGNMENT must NOT be back in the output path. This
    // targets the assignment specifically (not the bare token, which legitimately
    // appears in the explanatory comment and in the unrelated ndi-filter.cpp path) so
    // a silent subtree revert that restores `video_frame.timecode = synthesize` fails
    // HERE — the exact ~150 ms-lag regression (#76) this guard protects against.
    assert!(
        !src.contains("video_frame.timecode = NDIlib_send_timecode_synthesize"),
        "{NDI_OUTPUT}: #78 REVERTED — the video output frame is stamping the stock \
         `NDIlib_send_timecode_synthesize` counter again, reintroducing the ~150 ms \
         emit-timecode lag (#76). Re-apply the real-wall-clock timecode patch."
    );
    assert!(
        !src.contains("audio_frame.timecode = NDIlib_send_timecode_synthesize"),
        "{NDI_OUTPUT}: #78 REVERTED — the audio output frame is stamping the stock \
         `NDIlib_send_timecode_synthesize` counter again. Re-apply the \
         real-wall-clock timecode patch."
    );
}

#[test]
fn windows_genlock_workflow_gates_on_the_timecode_patch() {
    // #85: the Windows production-build workflow must gate the artifact on the #78
    // SOURCE patch — the real-wall-clock emit timecode — BEFORE the 150-min build, so a
    // future `git subtree pull` (#44) that restores `synthesize` fails fast rather than
    // shipping a binary that reintroduces the ~150 ms lag. The canonical guard is the
    // test above, but this crate is Linux-only (v4l/alsa/evdev) and cannot compile on
    // the windows-2022 runner, so the workflow re-asserts the same source tokens in
    // pwsh. This test keeps that pwsh gate in lock-step with the canonical assertion:
    // drop the source check from the workflow and CI fails here. (Same lock-step
    // convention as tests/obs_updater_disabled.rs and tests/genlock_preload.rs.)
    let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));

    assert!(
        wf.contains("video_frame.timecode = genlock_emit_timecode_100ns("),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts the #78 \
         real-wall-clock video emit-timecode SOURCE patch — a future subtree bump \
         could restore `synthesize` and reship the ~150 ms lag while the version pin \
         still passes. Re-add the pwsh source-patch gate."
    );
    assert!(
        wf.contains("audio_frame.timecode = genlock_wall_now_100ns()"),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts the #78 \
         real-wall-clock audio emit-timecode SOURCE patch (#85). Re-add the pwsh \
         source-patch gate."
    );
    assert!(
        wf.contains("video_frame.timecode = NDIlib_send_timecode_synthesize"),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts that the stock \
         `synthesize` assignment is ABSENT from the output path (#85) — the negative \
         guard that catches a silent #78 revert. Re-add the pwsh source-patch gate."
    );
    assert!(
        wf.contains("return genlock_floor_boundary_100ns(genlock_wall_now_100ns(), fps);"),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts the #1009 FLOOR emit \
         stamp — a subtree bump could restore the future-biased ceil stamp (the issue-1007 \
         hair-trigger arming) while the version pin still passes. Re-add the pwsh #1009 gate."
    );
}
