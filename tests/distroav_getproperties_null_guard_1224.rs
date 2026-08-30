//! #1224 — strih OBS c0000005 in `obs.dll!new_prop`, reached from DistroAV's
//! `ndi_source_getproperties` (the ONLY distroav path to `new_prop` — `ndi_source_update` never
//! builds `obs_properties`, so the dump's `ndi_source_update+0x1e8`/`new_ndi_receiver_name+0xa2`
//! frames are offset-symbolized "nearest export", not the true crashing function).
//!
//! Root hazard: `finder.getNDISourceList(finder_callback)` runs the callback on a DETACHED thread
//! (`vendor/distroav/src/ndi-finder.cpp`: `std::thread(refreshNDISourceList, callback).detach()`),
//! firing 5+ s later (5 s throttle + a `find_wait_for_sources(…,1000)` loop) — long after
//! `ndi_source_getproperties` returned its `props`. The lambda captures RAW `source_list`/`s` and
//! then calls `obs_source_update_properties(s->obs_source)`, re-triggering a fresh
//! `ndi_source_getproperties` → `obs_properties_add_*` → `new_prop`. Under the WS enforce/heal
//! `SetInputSettings` churn on a 4.7 s render-stalled graph (the OBS-log context) the neguarded
//! consumers are a NULL-deref / use-after-free.
//!
//! Fix class = `vendored-obs-frontend-crash-safety.md` #773 (guard-at-the-CONSUMER, early-return +
//! loud `[distroav]` warn). Purely additive NULL guards, no happy-path behavior change → a
//! **Rust source-anchor test ONLY, NO pwsh mirror** (that rule: a pure crash/NULL-safety guard is
//! Rust-anchor-only; only a rig-critical BEHAVIORAL divergence gets a windows-genlock*.yml mirror).
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored C++ compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md`
//! (#1026 recipe) this file source-ANCHORS the guard tokens with a `fs::read_to_string` check
//! runnable via `rustc --test --edition 2021` — revert protection against a future `git subtree
//! pull` and the local RED→GREEN. The true behavioral repro (a live NULL/UAF under a render stall)
//! is NOT locally reproducible (vendored code compiles on CI, crash reproduces only on the live rig).

use std::fs;
use std::path::PathBuf;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

#[test]
fn getproperties_null_guards_present_1224() {
    let src = vendor_file(NDI_SOURCE);

    // (A) genlock_ensure_saved_source_listed + (C) the detached finder callback BOTH carry the
    //     source_list-aware guard (the pre-#1224 form guarded only `!s`, never `!source_list`).
    //     The distinctive `!source_list ||` prefix did not exist before this fix → clean RED→GREEN.
    let guard_count = src.matches("!source_list || !s || !s->obs_source").count();
    assert_eq!(
        guard_count, 2,
        "expected the #1224 source_list-aware NULL guard at BOTH genlock_ensure_saved_source_listed \
         and the detached finder callback; found {guard_count}"
    );

    // (B) props guard-at-consumer BEFORE any obs_properties composition (new_prop).
    assert!(
        src.contains("[distroav] ndi_source_getproperties: obs_properties_create returned NULL"),
        "missing the #1224 obs_properties_create NULL guard warn in ndi_source_getproperties"
    );

    // (C) the async finder callback guards its captured source_list/s before ANY deref.
    assert!(
        src.contains("[distroav] ndi finder callback: NULL/stale source_list"),
        "missing the #1224 detached-finder-callback NULL/stale guard warn"
    );

    // Regression: the #795 call site anchored by BOTH windows-genlock*.yml must stay intact at
    // both sites (my guards go into the function body + the lambda head, never the call text).
    let call_count = src.matches("genlock_ensure_saved_source_listed(source_list, s)").count();
    assert_eq!(
        call_count, 2,
        "the #795 genlock_ensure_saved_source_listed(source_list, s) call site (windows-genlock*.yml \
         anchor) must stay intact at both call sites; found {call_count}"
    );
}
