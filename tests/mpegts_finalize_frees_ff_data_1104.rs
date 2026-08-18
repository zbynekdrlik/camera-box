//! #1104 — regression gate: the vendored mpegts output must free `ff_data` on EVERY
//! failed-start early-return in `ffmpeg_mpegts_finalize`.
//!
//! Background (see the ticket's validation + design comments): `ffmpeg_mpegts_finalize()` in
//! `vendor/obs-studio/plugins/obs-ffmpeg/obs-ffmpeg-mpegts.c` has four early exits after
//! `ffmpeg_mpegts_data_init()` allocates the AVFormatContext. Only the `data_init`-failure exit
//! calls `ffmpeg_mpegts_data_free(stream, &stream->ff_data)`; the three post-init failure branches
//! (`init_streams`, `open_output_file`, `pthread_create`) `return false` without freeing, and the
//! caller's `set_config()` `fail:` → `stop()` skips `ffmpeg_mpegts_full_stop()` because `active()`
//! is false on a failed start — so the half-built context (and, on the pthread branch, an already
//! connected SRT/RIST avio) leaks. The SRT-unreachable path (`error("Failed to open the url")`) is
//! the exact one the 2026-07-19 live crash log ends on.
//!
//! This is a STATIC text gate (the vendored C compiles only on CI — the same idiom as
//! `aux_sender_teardown_ordering_877.rs` / `genlock_release_cadence.rs` for vendored code). It reads
//! the SHIPPED bytes and asserts each failure branch frees `ff_data` before it bails. Per
//! `vendored-libobs-change-safety.md` a gate is a lie until watched go red, so
//! `mutation_proof_checker_distinguishes_free_from_no_free` runs the SAME checker over synthetic
//! fixtures (a "no free" branch that MUST be rejected + a "has free" branch that MUST pass) — the
//! mutation proof, baked in and independent of the real file.
//!
//! Local RED->GREEN (no cargo needed — camera-box's Tier-0 build-ok bypass is disabled, issue 1026):
//!   CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 \
//!       tests/mpegts_finalize_frees_ff_data_1104.rs -o /tmp/t && /tmp/t
//!   (exit 101 = RED before the fix, 0 = GREEN after.)

use std::path::PathBuf;

fn mpegts_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor/obs-studio/plugins/obs-ffmpeg/obs-ffmpeg-mpegts.c");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("#1104: cannot read {}: {e}", p.display()))
}

/// Slice the body of a top-level C function: from the DEFINITION line `sig` to the first
/// column-0 closing brace (`"\n}"`). Inner blocks in this function close tab-indented (`"\n\t}"`),
/// so `"\n}"` reliably marks the function's own end. Scoping to the enclosing function (never a
/// fixed byte window) is the anchor-safety rule from `vendored-libobs-change-safety.md`.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("#1104: signature anchor not found: {sig:?}"));
    let rest = &src[start..];
    let end = rest
        .find("\n}")
        .unwrap_or_else(|| panic!("#1104: no column-0 closing brace after {sig:?}"));
    &rest[..end]
}

/// True iff, within `body`, the window from `branch_anchor` to the FIRST `return false;` after it
/// contains a `ffmpeg_mpegts_data_free` call — i.e. "this failure branch frees ff_data before it
/// bails out". Returns false if the anchor is missing, no `return false;` follows it, or nothing
/// frees in between — the discriminating predicate the mutation proof exercises.
fn frees_ff_data_before_bail(body: &str, branch_anchor: &str) -> bool {
    let a = match body.find(branch_anchor) {
        Some(i) => i,
        None => return false,
    };
    let rest = &body[a..];
    let r = match rest.find("return false;") {
        Some(i) => i,
        None => return false,
    };
    rest[..r].contains("ffmpeg_mpegts_data_free")
}

const FINALIZE_SIG: &str = "static bool ffmpeg_mpegts_finalize(";
// Each anchor is a string literal that is UNIQUE inside the finalize body (the "or invalid stream"
// variant of the open-url error lives in open_output_file, outside this slice).
const BRANCH_INIT_STREAMS: &str = "mpegts avstream failed to be created";
const BRANCH_OPEN_URL: &str = "Failed to open the url";
const BRANCH_WRITE_THREAD: &str = "Failed to create write thread.";

#[test]
fn init_streams_failure_branch_frees_ff_data() {
    let src = mpegts_src();
    let body = fn_body(&src, FINALIZE_SIG);
    assert!(
        body.contains(BRANCH_INIT_STREAMS),
        "#1104: init_streams failure anchor {BRANCH_INIT_STREAMS:?} not found in finalize body"
    );
    assert!(
        frees_ff_data_before_bail(body, BRANCH_INIT_STREAMS),
        "#1104: the init_streams failure branch returns false WITHOUT ffmpeg_mpegts_data_free — ff_data leaks"
    );
}

#[test]
fn open_output_file_failure_branch_frees_ff_data() {
    let src = mpegts_src();
    let body = fn_body(&src, FINALIZE_SIG);
    assert!(
        body.contains(BRANCH_OPEN_URL),
        "#1104: open-url failure anchor {BRANCH_OPEN_URL:?} not found in finalize body"
    );
    assert!(
        frees_ff_data_before_bail(body, BRANCH_OPEN_URL),
        "#1104: the open_output_file failure branch (the SRT-unreachable crash path) returns false \
         WITHOUT ffmpeg_mpegts_data_free — ff_data leaks"
    );
}

#[test]
fn write_thread_failure_branch_frees_ff_data() {
    let src = mpegts_src();
    let body = fn_body(&src, FINALIZE_SIG);
    assert!(
        body.contains(BRANCH_WRITE_THREAD),
        "#1104: write-thread failure anchor {BRANCH_WRITE_THREAD:?} not found in finalize body"
    );
    assert!(
        frees_ff_data_before_bail(body, BRANCH_WRITE_THREAD),
        "#1104: the pthread_create failure branch returns false WITHOUT ffmpeg_mpegts_data_free — \
         an already-connected SRT/RIST avio leaks"
    );
}

/// Mutation proof (baked in, always runs): the checker MUST reject a branch that bails without a
/// free, and MUST accept one that frees before the return. Keeps the gate honest even once the real
/// file is GREEN — "a gate is a lie until you watch it go red".
#[test]
fn mutation_proof_checker_distinguishes_free_from_no_free() {
    let no_free = "\t\t\terror(\"boom\");\n\t\t\t*code = OBS_OUTPUT_ERROR;\n\t\t\treturn false;\n";
    let with_free = "\t\t\terror(\"boom\");\n\t\t\tffmpeg_mpegts_data_free(stream, &stream->ff_data);\n\t\t\treturn false;\n";
    assert!(
        !frees_ff_data_before_bail(no_free, "boom"),
        "#1104: checker must REJECT a failure branch that returns false with no free"
    );
    assert!(
        frees_ff_data_before_bail(with_free, "boom"),
        "#1104: checker must ACCEPT a failure branch that frees ff_data before returning"
    );
}
