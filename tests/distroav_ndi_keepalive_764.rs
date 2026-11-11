//! Patch-presence guard for #764 (event-critical, 2026-07-15) — genlocked NDI cam sources on
//! strih/imag must NEVER sleep/reconnect when hidden.
//!
//! Background: stock DistroAV's NDI source receiver thread does two things that combine into
//! a real reconnect penalty on every program cut to a previously-hidden camera:
//!   1. `ndi_source_hidden()` fully tears down the receiver thread (`ndi_source_thread_stop`
//!      -> `ndiLib->recv_destroy`) unless the source's own `behavior` setting is
//!      `PROP_BEHAVIOR_KEEP_ACTIVE` — our #150/#257 lockdown was FORCING every genlocked
//!      source to `PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME` instead, so this fired on every hide.
//!   2. Even with `KEEP_ACTIVE`, stock DistroAV's `ndi_source_thread` loop still SKIPS pulling
//!      frames (`if (!obs_source_showing(...)) { sleep(5ms); continue; }`) while hidden — an
//!      FPS-preserving optimization that conflates cheap DECODE cost with expensive GPU
//!      UPLOAD cost (upload only ever happens for a source actually being rendered).
//!
//! The #764 fix: (1) the #150/#257 forced-settings table now forces `PROP_BEHAVIOR_KEEP_ACTIVE`
//! instead of `STOP_RESUME_LAST_FRAME` for genlocked sources; (2) the thread loop's hidden-skip
//! is bypassed (decode+output keeps running) whenever `genlock_source_is_active()` (a runtime-
//! resolved wrapper over our own core `obs_source_get_genlock_fifo` export) reports true for
//! that source. Both changes are SCOPED to genlocked sources only — a non-genlock/aux NDI
//! input, or this code running against a stock (unpatched) OBS core where the getter resolves
//! to nullptr, keeps stock DistroAV's original behavior unchanged.
//!
//! This is a SOURCE-level guard, not a runtime test (same convention as
//! tests/obs_updater_disabled.rs / tests/genlock_preload.rs's vendored-source facet): the
//! patch lives in vendored C++ (`vendor/distroav/src/ndi-source.cpp`), and a future
//! `git subtree pull` upstream bump (the `/update-av-stack` flow) could silently re-import
//! stock DistroAV's original hide/deactivate handlers. If that happens, CI fails loudly here.

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
fn forced_behavior_is_keep_active_not_stop_resume() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // The #764 patch: the GENLOCK_FORCED_SETTINGS table entry for PROP_BEHAVIOR must be
    // PROP_BEHAVIOR_KEEP_ACTIVE.
    assert!(
        src.contains("{PROP_BEHAVIOR, false, PROP_BEHAVIOR_KEEP_ACTIVE, false}"),
        "{NDI_SOURCE}: #764 patch missing — GENLOCK_FORCED_SETTINGS must force \
         PROP_BEHAVIOR_KEEP_ACTIVE (was PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME) so \
         ndi_source_hidden() never tears down a genlocked source's receiver thread. A \
         `git subtree pull` upstream bump likely reverted it; re-apply the #764 patch."
    );

    // And the pre-#764 forced value must be gone from that SPECIFIC table entry — catches a
    // silent revert. (PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME legitimately still appears
    // elsewhere in the file — e.g. the UI's own default and the legacy-migration branch — so
    // this checks the EXACT forced-table tuple, not a bare substring of the whole file.)
    assert!(
        !src.contains("{PROP_BEHAVIOR, false, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME, false}"),
        "{NDI_SOURCE}: the pre-#764 forced behavior (STOP_RESUME_LAST_FRAME) is back in the \
         GENLOCK_FORCED_SETTINGS table — the #764 keep-alive patch was reverted. Re-apply it."
    );
}

#[test]
fn get_genlock_fifo_is_runtime_resolved() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // #764: a GETTER resolver mirroring the existing resolve_set_genlock_fifo pattern (same
    // rationale: DistroAV builds against stock SDK headers with no genlock symbols, so a
    // link-time call to obs_source_get_genlock_fifo cannot build).
    assert!(
        src.contains("resolve_get_genlock_fifo"),
        "{NDI_SOURCE}: #764 patch missing — no runtime-resolved getter for \
         obs_source_get_genlock_fifo found. Without it, the receiver thread cannot tell a \
         genlocked source from a non-genlock one at runtime."
    );
    assert!(
        src.contains(r#"resolve_obs_export("obs_source_get_genlock_fifo")"#),
        "{NDI_SOURCE}: the getter must resolve the EXACT core export name \
         \"obs_source_get_genlock_fifo\" (mirrors obs.h's EXPORT declaration) — a typo here \
         would silently resolve to nullptr and disable the whole #764 fix."
    );
    assert!(
        src.contains("genlock_source_is_active"),
        "{NDI_SOURCE}: #764 patch missing — no genlock_source_is_active() wrapper found. The \
         receiver thread's hidden-skip bypass must go through this single, honestly-fails-\
         closed (false on a stock/unpatched OBS) helper, never a raw resolver call inline."
    );
}

#[test]
fn receiver_thread_keepalive_bypass_present_in_vendored_source() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // #764: the hidden-source skip in the receiver thread loop must be conditioned on
    // genlock_source_is_active — i.e. a genlocked source's hidden-skip is bypassed.
    assert!(
        src.contains(
            "if (!obs_source_showing(s->obs_source) && !genlock_source_is_active(s->obs_source)) {"
        ),
        "{NDI_SOURCE}: #764 patch missing — the receiver thread's hidden-source frame-skip \
         must be gated on `!genlock_source_is_active(s->obs_source)` too, so a genlocked \
         source keeps decoding+outputting frames while hidden instead of pausing every ~5ms \
         loop tick. A `git subtree pull` upstream bump likely reverted this; re-apply it."
    );

    // The stock (pre-#764) unconditional skip must be gone as its own standalone condition —
    // catches a silent revert to the original stock check.
    assert!(
        !src.contains("if (!obs_source_showing(s->obs_source)) { // Avoid busy-waiting"),
        "{NDI_SOURCE}: the stock unconditional hidden-source skip is back (no genlock \
         exception) — the #764 keep-alive patch was reverted. Re-apply it."
    );
}

#[test]
fn keepalive_log_line_present_exactly_once_per_source() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // #764: the exact requested log line, gated on the "logged once" flag so it never spams
    // the ~5ms-cadence loop.
    assert!(
        src.contains(r#"obs_log(LOG_INFO, "genlock: NDI receiver keep-alive (no sleep on hide) '%s'", obs_source_name);"#),
        "{NDI_SOURCE}: #764 patch missing — the exact requested keep-alive log line \
         (\"genlock: NDI receiver keep-alive (no sleep on hide)\") was not found."
    );
    assert!(
        src.contains("logged_genlock_keepalive"),
        "{NDI_SOURCE}: #764 patch missing — no per-source \"already logged\" guard field \
         found. Without it the keep-alive line would spam every ~5ms loop tick instead of \
         firing exactly once per source."
    );
}

#[test]
fn ndi_source_t_struct_carries_the_logged_flag() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // The flag must live INSIDE the ndi_source_t struct (not a stray global), so each source
    // instance tracks its own "already logged" state independently.
    let struct_start = src
        .find("typedef struct ndi_source_t {")
        .expect("ndi_source_t struct definition must exist");
    let struct_end = src[struct_start..]
        .find("} ndi_source_t;")
        .map(|off| struct_start + off)
        .expect("ndi_source_t struct must close with '} ndi_source_t;'");
    let struct_body = &src[struct_start..struct_end];
    assert!(
        struct_body.contains("bool logged_genlock_keepalive;"),
        "ndi_source_t struct body does not declare `bool logged_genlock_keepalive;` — the \
         #764 per-source once-only log guard must live inside this struct: {struct_body}"
    );
}
