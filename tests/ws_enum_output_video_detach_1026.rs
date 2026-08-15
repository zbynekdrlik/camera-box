//! #1026 — Tier-0 source-anchor guard for the obs-websocket `GetOutputList` SIGSEGV fix.
//!
//! Root cause (see the ticket's design comment): a raw output's borrowed `output->video`
//! (set to `obs_get_video()` at `obs_output_create`, never cleared on stop) is left dangling
//! when the main canvas mix video is freed in `obs_canvas_clear_mix`. obs-websocket's
//! `GetOutputList` → `obs_output_get_width` → `video_output_get_width` → `get_const_root` then
//! walks the freed+reused `video_t` on a Qt WS worker thread and SIGSEGVs (imag-nb: 7 crashes
//! in ~32h on the matched full bundle). The #793 fix detaches `canvas->mix` but NOT the copies
//! outputs already took into `output->video`.
//!
//! The fix detaches every `output->video == old_mix->video` (under `outputs_mutex`) BEFORE the
//! mix video is freed — extending #793's detach-before-free invariant to the borrowed output
//! copies. The vendored C compiles only on CI, so per
//! `.claude/rules/vendored-libobs-change-safety.md` this SOURCE-ANCHOR test pins the fix in
//! place so a later refactor cannot silently drop it. It fails LOUDLY if the file is missing
//! (test-strictness: a guard that silently passes without running is worse than none).
//!
//! Anchors are kept SHORT and wrap-independent, and scoped to the `obs_canvas_clear_mix`
//! function body, per the anchor-safety notes in `vendored-libobs-change-safety.md`.

use std::fs;
use std::path::PathBuf;

const OBS_CANVAS: &str = "vendor/obs-studio/libobs/obs-canvas.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read_obs_canvas() -> String {
    let path = repo(OBS_CANVAS);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Slice `obs_canvas_clear_mix`'s body: from its signature to the next top-level function
/// (a line starting with `void ` / `bool ` / `obs_` at column 0). Scoping to the enclosing
/// function — never a fixed byte window — is the anchor-safety rule from
/// `vendored-libobs-change-safety.md`.
fn clear_mix_body(src: &str) -> &str {
    let start = src
        .find("void obs_canvas_clear_mix(obs_canvas_t *canvas)")
        .expect("obs_canvas_clear_mix definition not found — did the signature change?");
    let after = &src[start..];
    // First top-level function that follows (deindented `void ` at line start after a `\n`).
    let end_rel = after
        .match_indices("\nvoid obs_free_canvas_mixes")
        .next()
        .map(|(i, _)| i)
        .unwrap_or(after.len());
    &after[..end_rel]
}

#[test]
fn clear_mix_detaches_borrowed_output_video_1026() {
    let src = read_obs_canvas();
    let body = clear_mix_body(&src);

    // 1. The detach takes the SAME lock obs_enum_outputs walks under.
    assert!(
        body.contains("obs->data.outputs_mutex"),
        "#1026: obs_canvas_clear_mix must lock obs->data.outputs_mutex to detach borrowed \
         output->video before freeing the mix video"
    );

    // 2. It clears the borrowed pointer.
    assert!(
        body.contains("output->video = NULL"),
        "#1026: obs_canvas_clear_mix must NULL output->video for outputs borrowing the freed mix"
    );

    // 3. It clears only outputs that borrowed THIS mix's video (not all outputs).
    assert!(
        body.contains("output->video == freed_video"),
        "#1026: the detach must match output->video against the freed mix video (freed_video)"
    );

    // 4. It walks the real output list head.
    assert!(
        body.contains("obs->data.first_output"),
        "#1026: the detach must walk obs->data.first_output (the list obs_enum_outputs walks)"
    );
}

#[test]
fn detach_runs_before_mix_free_no_lock_nesting_1026() {
    let src = read_obs_canvas();
    let body = clear_mix_body(&src);

    let outputs_lock = body.find("obs->data.outputs_mutex").expect(
        "#1026: outputs_mutex detach missing (see clear_mix_detaches_borrowed_output_video_1026)",
    );
    let mixes_lock = body
        .find("obs->video.mixes_mutex")
        .expect("#1026: obs_canvas_clear_mix must still lock mixes_mutex to free the mix");

    // The borrowed-pointer detach must happen BEFORE the mixes_mutex critical section, so
    // outputs_mutex is never taken while holding mixes_mutex (no lock nesting / no deadlock
    // ordering to reason about). Rejected alternative #2 in the design comment.
    assert!(
        outputs_lock < mixes_lock,
        "#1026: the outputs_mutex detach must run BEFORE the mixes_mutex free (no nested locking)"
    );
}
