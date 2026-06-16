//! Patch-presence guard for #93 — DistroAV NDI source-name use-after-free fix.
//!
//! Background (root-caused 2026-06-16, read-only crash-dump + WER + production-log
//! triage): strih OBS crashed with `c0000374 STATUS_HEAP_CORRUPTION` cascading into
//! `c0000005` while OUR phase2-probe harness re-pointed a LIVE NDI source over
//! obs-websocket. The mechanism is a classic data race on the source-name strings in
//! `vendor/distroav/src/ndi-source.cpp`:
//!
//!   * `ndi_source_update` (called on the UI / obs-websocket thread) does
//!     `bfree(s->config.ndi_source_name)` then `bstrdup(new_ndi_source_name)` on
//!     EVERY update, and `on_ndi_source_renamed` reallocs `ndi_receiver_name`.
//!   * the A/V thread (`ndi_source_thread`) borrows those exact `char*` pointers
//!     into `recv_desc` (`recv_desc.source_to_connect_to.p_ndi_name = s->config.
//!     ndi_source_name`) inside the `reset_ndi_receiver` block and passes them to
//!     `recv_create_v3`.
//!
//! When the harness re-points a LIVE source, `update` frees the string the A/V thread
//! is mid-read on → heap corruption → OBS crash (the exact c0000374 we captured). The
//! OBS async frame queue is ALREADY protected by `source->async_mutex`, so the genlock
//! RENDER path is safe — the fix must NOT lock the render path.
//!
//! #93 fix (Option B, "most complete"):
//!   1. A per-source `pthread_mutex_t config_mutex` serialises the config-mutation
//!      section of `ndi_source_update` against the `reset_ndi_receiver` block of the
//!      A/V thread (held for microseconds, NEVER across the blocking recv_capture).
//!   2. Defense-in-depth: the A/V thread takes its OWN `bstrdup` copies of the name
//!      strings inside the locked reset block and binds recv_desc to those owned
//!      copies — so a future caller that mutates config WITHOUT the lock still cannot
//!      UAF the A/V thread.
//!   3. The mutex is init/destroyed in source create/destroy.
//!
//! This is a SOURCE-level guard, not a runtime test (same convention as
//! tests/distroav_timecode_patch.rs, tests/genlock_preload.rs and
//! tests/obs_updater_disabled.rs): the genlock patch lives in the vendored C++
//! (`git log -- vendor/` is the patch series, per vendor/README.md). The risk it
//! defends against is a future `git subtree pull` upstream release-bump (the
//! `/update-av-stack` flow, #44) silently restoring the unsynchronised
//! free/realloc-vs-borrow and reintroducing the #93 heap-corruption crash —
//! which `scripts/drift-guard.sh` would NOT catch (it pins the DistroAV VERSION,
//! not fork-patch CONTENT). If the patch reverts, CI fails loudly HERE.

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

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

#[test]
fn source_struct_has_config_mutex() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // The per-source config lock must be declared in ndi_source_t.
    assert!(
        src.contains("pthread_mutex_t config_mutex"),
        "{NDI_SOURCE}: #93 patch missing — `pthread_mutex_t config_mutex` is not \
         declared in ndi_source_t. A `git subtree pull` (#44) likely reverted the \
         source-name UAF fix; re-apply it."
    );
}

#[test]
fn config_mutex_is_initialised_and_destroyed() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("pthread_mutex_init(&s->config_mutex"),
        "{NDI_SOURCE}: #93 — config_mutex is never pthread_mutex_init'd in \
         ndi_source_create. Re-apply the #93 fix."
    );
    assert!(
        src.contains("pthread_mutex_destroy(&s->config_mutex"),
        "{NDI_SOURCE}: #93 — config_mutex is never pthread_mutex_destroy'd in \
         ndi_source_destroy. Re-apply the #93 fix."
    );
}

#[test]
fn ndi_source_update_locks_config_mutation() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // ndi_source_update must take and release the config lock around the
    // string free/realloc + scalar config writes + thread start/stop.
    let locks = src.matches("pthread_mutex_lock(&s->config_mutex").count();
    let unlocks = src.matches("pthread_mutex_unlock(&s->config_mutex").count();
    assert!(
        locks >= 2 && unlocks >= 2,
        "{NDI_SOURCE}: #93 — expected config_mutex to be locked at BOTH sites \
         (ndi_source_update config-mutation AND the av_thread reset_ndi_receiver \
         block); found {locks} lock / {unlocks} unlock call(s). The source-name UAF \
         fix (#93) is incomplete or reverted; re-apply it."
    );
}

#[test]
fn av_thread_uses_owned_string_copies_not_borrowed_config_pointers() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // The A/V thread must own DUPLICATED copies of the name strings (defense in
    // depth): bstrdup'd into thread-owned storage inside the locked reset block, and
    // recv_desc must bind to THOSE owned copies — never the live config.* pointers
    // that ndi_source_update frees.
    assert!(
        src.contains("owned_source_name") && src.contains("owned_receiver_name"),
        "{NDI_SOURCE}: #93 — the av_thread no longer keeps OWNED copies \
         (owned_source_name / owned_receiver_name) of the NDI name strings. \
         recv_desc would borrow the live config.* pointers that ndi_source_update \
         frees → use-after-free. Re-apply the #93 defense-in-depth copies."
    );
    assert!(
        src.contains("recv_desc.source_to_connect_to.p_ndi_name = owned_source_name"),
        "{NDI_SOURCE}: #93 — recv_desc.source_to_connect_to.p_ndi_name is bound to a \
         pointer other than the av_thread-owned copy (owned_source_name). It must NOT \
         borrow s->config.ndi_source_name (freed by ndi_source_update → UAF). \
         Re-apply the #93 fix."
    );
    assert!(
        src.contains("recv_desc.p_ndi_recv_name = owned_receiver_name"),
        "{NDI_SOURCE}: #93 — recv_desc.p_ndi_recv_name is bound to a pointer other \
         than the av_thread-owned copy (owned_receiver_name). It must NOT borrow \
         s->config.ndi_receiver_name (reallocated by on_ndi_source_renamed → UAF). \
         Re-apply the #93 fix."
    );

    // The stock (vulnerable) direct borrow of the live config pointers into recv_desc
    // must be GONE from the reset block — its presence is the exact #93 UAF.
    assert!(
        !src.contains("recv_desc.source_to_connect_to.p_ndi_name = s->config.ndi_source_name"),
        "{NDI_SOURCE}: #93 REVERTED — recv_desc.source_to_connect_to.p_ndi_name is \
         borrowing the live s->config.ndi_source_name again (the stock code). \
         ndi_source_update frees that string while the av_thread reads it → heap \
         corruption (#93). Re-apply the owned-copy fix."
    );
    assert!(
        !src.contains("recv_desc.p_ndi_recv_name = s->config.ndi_receiver_name"),
        "{NDI_SOURCE}: #93 REVERTED — recv_desc.p_ndi_recv_name is borrowing the live \
         s->config.ndi_receiver_name again (the stock code). on_ndi_source_renamed \
         reallocs that string while the av_thread reads it → heap corruption (#93). \
         Re-apply the owned-copy fix."
    );
}
