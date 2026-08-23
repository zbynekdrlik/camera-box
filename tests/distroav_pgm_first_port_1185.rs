//! Patch-presence guard for #1185 — DistroAV pins the program (2ME PGM) NDI sender to the
//! FIRST NDI port (:5961) via an early module-post-load RESERVATION that the real
//! `ndi_output_start` then ADOPTS.
//!
//! Background: libndi assigns each `NDIlib_send_create` a TCP port sequentially from 5961 in
//! CREATION ORDER. Stock DistroAV (`vendor/distroav/src/plugin-main.cpp`) defers
//! `main_output_init()`/`preview_output_init()` to `OBS_FRONTEND_EVENT_FINISHED_LOADING` via
//! `QMetaObject::invokeMethod(..., Qt::QueuedConnection)` — i.e. AFTER the scene collection
//! loads — so the per-source `ndi_filter` republishes (Grading/MULTIVIEW/interkom) win the low
//! ports and the program output lands on a HIGH one. A stock NDI Studio Monitor / building TV
//! that reconnects by CACHED PORT is then handed the wrong sender for the program after any OBS
//! restart (#1180/#1181). The #1185 fix RESERVES the program's send instance at
//! `obs_module_post_load` (which runs BEFORE scene load) so it grabs :5961, and has
//! `ndi_output_start` ADOPT that reserved instance instead of calling `send_create` again.
//!
//! This is a SOURCE-level guard, not a runtime test (same convention as
//! tests/distroav_timecode_patch.rs / tests/genlock_preload.rs): the fix lives in the vendored
//! C++ (`git log -- vendor/` is the patch series, per vendor/README.md). The risk it defends
//! against is a future `git subtree pull` upstream release-bump (the `/update-av-stack` flow,
//! #44) silently dropping the reservation and reintroducing the port reshuffle — which
//! `scripts/drift-guard.sh` would NOT catch (it pins the DistroAV VERSION, not fork-patch
//! CONTENT). If the patch reverts, CI fails loudly HERE — the "report conflicts loudly" contract
//! of the monorepo (#85).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. a clang-format wrap or an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
const PLUGIN_MAIN: &str = "vendor/distroav/src/plugin-main.cpp";
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";
const WINDOWS_GENLOCK_FAST_WF: &str = ".github/workflows/windows-genlock-fast.yml";

#[test]
fn ndi_output_defines_the_pgm_first_port_reservation() {
    let src = squish(&vendor_file(NDI_OUTPUT));

    // The reservation subsystem the #1185 fix introduces MUST be present.
    assert!(
        src.contains("void ndi_output_reserve_main_sender("),
        "{NDI_OUTPUT}: #1185 — `ndi_output_reserve_main_sender` (the early port reservation \
         created at obs_module_post_load) is gone. A `git subtree pull` (#44) likely reverted the \
         PGM-first-port patch; re-apply it."
    );
    assert!(
        src.contains("void ndi_output_release_reserved_main_sender("),
        "{NDI_OUTPUT}: #1185 — `ndi_output_release_reserved_main_sender` (the unadopted-reservation \
         cleanup) is gone; re-apply the PGM-first-port patch."
    );
    assert!(
        src.contains("ndi_output_take_reserved_sender("),
        "{NDI_OUTPUT}: #1185 — the `ndi_output_take_reserved_sender` adoption helper is gone; \
         re-apply the PGM-first-port patch."
    );

    // The adoption MUST be wired into the output start path (adopt-the-reserved-instance instead
    // of always creating a fresh one). This is the load-bearing line — without it the reservation
    // just leaks a port and the program still lands on a high one.
    assert!(
        src.contains("o->ndi_sender = ndi_output_take_reserved_sender("),
        "{NDI_OUTPUT}: #1185 — ndi_output_start no longer adopts the reserved main sender \
         (`o->ndi_sender = ndi_output_take_reserved_sender(...)`); the PGM-first-port pin is not \
         wired into the start path. Re-apply the patch."
    );
}

#[test]
fn module_post_load_reserves_the_main_output_port() {
    let src = squish(&vendor_file(PLUGIN_MAIN));

    // The reservation MUST be CALLED from obs_module_post_load — the whole point is that this runs
    // BEFORE the scene collection loads, so the sender grabs :5961 ahead of the ndi_filter senders.
    assert!(
        src.contains("void obs_module_post_load(void)"),
        "{PLUGIN_MAIN}: obs_module_post_load is gone/renamed — the #1185 reservation has no home."
    );
    assert!(
        src.contains("ndi_output_reserve_main_sender("),
        "{PLUGIN_MAIN}: #1185 — obs_module_post_load no longer calls ndi_output_reserve_main_sender; \
         the program NDI port is no longer reserved before scene load. Re-apply the patch."
    );
    // Gated on the main output being enabled (so a disabled PGM is never advertised frameless).
    assert!(
        src.contains("config->OutputEnabled"),
        "{PLUGIN_MAIN}: #1185 — the reservation is no longer gated on config->OutputEnabled; a \
         disabled PGM could be advertised. Re-apply the config gate."
    );
    // The unadopted reservation must be cleaned up on module unload.
    assert!(
        src.contains("ndi_output_release_reserved_main_sender()"),
        "{PLUGIN_MAIN}: #1185 — obs_module_unload no longer releases an unadopted reserved main \
         sender; a disabled/never-adopted PGM reservation would leak the port. Re-apply the patch."
    );
}

#[test]
fn windows_genlock_workflows_gate_on_the_pgm_first_port_patch() {
    // The vendored crate is Linux-only and cannot compile on the windows-2022 runner, so both
    // Windows genlock workflows (the full build AND the FAST path, which ships distroav.dll for
    // hot-swap) re-assert the #1185 source tokens in pwsh — keeping this canonical Rust guard in
    // lock-step. Drop the source check from either workflow and CI fails HERE. (Same lock-step
    // convention as tests/distroav_timecode_patch.rs.)
    for wf in [WINDOWS_GENLOCK_WF, WINDOWS_GENLOCK_FAST_WF] {
        let w = squish(&vendor_file(wf));
        assert!(
            w.contains("ndi_output_reserve_main_sender("),
            "{wf}: the build no longer asserts the #1185 ndi_output_reserve_main_sender SOURCE \
             patch — a future subtree bump could drop the PGM-first-port reservation while the \
             version pin still passes. Re-add the pwsh source-patch gate."
        );
        assert!(
            w.contains("ndi_output_take_reserved_sender("),
            "{wf}: the build no longer asserts the #1185 ndi_output_take_reserved_sender adoption \
             SOURCE patch. Re-add the pwsh source-patch gate."
        );
    }
}
