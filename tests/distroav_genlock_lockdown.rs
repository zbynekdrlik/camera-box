//! #150 — lock down the vendored DistroAV `ndi_source` config surface.
//!
//! Root cause (diagnosed + confirmed live on strih 10.77.9.202, 2026-06-22): two
//! strih scenes pointed at the SAME NDI source `CAM1 (usb)`. The production input
//! `NDI cam5` decoded 100% at the strih output; the harness probe ingest
//! `phase2-probe-src` decoded 0 — SOLELY because the probe was an
//! INCOMPLETELY-configured `ndi_source`. The real (in-code) differences vs the
//! certified working input were: `ndi_recv_hw_accel` true → ABSENT (defaults to
//! false; no `getdefaults` entry exists for it), `ndi_audio` false → true (the
//! `getdefaults` entry forces it true), and `genlock_fifo` was relied on per-source.
//! Applying a FULL clone of `NDI cam5` onto the probe made strih decode 0 → 100%.
//!
//! The user's directive (#150): the more user-changeable NDI options, the more
//! failure points. A genlock NDI source must EXPOSE ONLY (1) the NDI source
//! selection and (2) the genlock preload (video delay), and HARDCODE every other
//! setting to the certified zero-loss values — at the CODE level, regardless of any
//! saved / UI value — so NO input, scene, human or harness can ever misconfigure it.
//! Every genlock source (prod, probe, or a newly-added one) is then correct by
//! construction. This SUPERSEDES the harness "full clone" workaround (#149).
//!
//! Certified genlock values (read live from the working prod input `NDI cam5`,
//! 2026-06-22): `ndi_sync` = 2 (SOURCE_TIMECODE / source timing), `genlock_fifo` =
//! true, `ndi_behavior` = 2 (STOP_RESUME_LAST_FRAME), `ndi_recv_hw_accel` = true,
//! `ndi_bw_mode` = 0 (highest), `latency` = 0 (NORMAL), `ndi_audio` = false,
//! `ndi_framesync` = false, `ndi_fix_alpha_blending` = false. The two user knobs
//! (`ndi_source_name`, `genlock_preload`) are NEVER forced.
//!
//! This is a SOURCE-presence guard (same convention as
//! tests/distroav_source_config_lock.rs, tests/genlock_preload.rs,
//! tests/obs_updater_disabled.rs): the lockdown lives in the vendored C++
//! (`git log -- vendor/` is the patch series, per vendor/README.md). It defends
//! against a future `git subtree pull` upstream bump (#44) silently dropping the
//! lockdown and reintroducing the #150 misconfig class — which
//! `scripts/drift-guard.sh` would NOT catch (it pins the DistroAV VERSION, not
//! fork-patch CONTENT). If the patch reverts, CI fails loudly HERE.

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
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";

/// Extract the body of a free function `name(` up to the next top-level `\n<ret> <name>(`
/// boundary. Good enough to scope an assertion to one function in this file.
fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} not found in {NDI_SOURCE}"));
    let rest = &src[start + signature.len()..];
    // End at the next line that begins a new top-level definition (a non-indented
    // identifier followed eventually by '('). The simple, robust marker used by the
    // sibling guards is the next "\n<type> <ident>(" — here we look for the next
    // function that is defined at column 0.
    let end = rest
        .find("\nobs_properties_t *ndi_source")
        .or_else(|| rest.find("\nvoid ndi_source"))
        .or_else(|| rest.find("\nvoid deactivate_source"))
        .or_else(|| rest.find("\nstatic "))
        .map(|i| start + signature.len() + i)
        .unwrap_or(src.len());
    &src[start..end]
}

/// The single helper that FORCES every certified genlock value must exist and set
/// each certified key to its certified value. Asserting the helper (not 9 scattered
/// `obs_data_set_*` lines) keeps the lockdown in one auditable place.
#[test]
fn genlock_certified_forcer_exists_and_sets_every_certified_key() {
    let src = squish(&vendor_file(NDI_SOURCE));

    assert!(
        src.contains("force_genlock_certified_settings"),
        "{NDI_SOURCE}: #150 — the certified-value forcer \
         `force_genlock_certified_settings(...)` is missing. A genlock source could \
         then come up with a saved/default value for sync/behavior/hw_accel/bw/\
         latency/audio/framesync/alpha and misconfigure the zero-loss path. Re-apply \
         the #150 lockdown."
    );

    // #257: the forcer is driven by the GENLOCK_FORCED_SETTINGS const table (the COMPLEMENT of the
    // whitelist), not individual obs_data_set calls. Every certified key MUST be a table entry so a
    // dropped/weakened force fails AND an upstream property add can't reintroduce a live knob.
    let must_force: &[&str] = &[
        "{PROP_SYNC, false, PROP_SYNC_NDI_SOURCE_TIMECODE, false}",
        "{PROP_BEHAVIOR, false, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME, false}",
        "{PROP_BANDWIDTH, false, PROP_BW_HIGHEST, false}",
        "{PROP_LATENCY, false, PROP_LATENCY_NORMAL, false}",
        "{PROP_TIMEOUT, false, PROP_TIMEOUT_KEEP_CONTENT, false}",
        "{PROP_YUV_RANGE, false, PROP_YUV_RANGE_PARTIAL, false}",
        "{PROP_YUV_COLORSPACE, false, PROP_YUV_SPACE_BT709, false}",
        "{PROP_HW_ACCEL, true, 0, true}",
        "{PROP_AUDIO, true, 0, false}",
        "{PROP_FRAMESYNC, true, 0, false}",
        "{PROP_FIX_ALPHA, true, 0, false}",
        "{PROP_PTZ, true, 0, false}",
    ];
    assert!(
        src.contains("GENLOCK_FORCED_SETTINGS"),
        "{NDI_SOURCE}: #257 — the GENLOCK_FORCED_SETTINGS const table is gone; re-apply the forcer table."
    );
    for needle in must_force {
        assert!(
            src.contains(needle),
            "{NDI_SOURCE}: #257 — GENLOCK_FORCED_SETTINGS does not pin `{needle}`. That key could be \
             left at a saved/UI/default value and misconfigure the genlock zero-loss path. Re-apply \
             the full forcer table."
        );
    }
}

/// `ndi_source_update` MUST invoke the forcer, gated on `genlock_fifo` being enabled,
/// BEFORE it reads the per-key settings into `s->config`. Gating on `genlock_fifo`
/// keeps non-genlock aux/preview inputs (NDI 2ME PVW / Bible / Camera info, ndi_sync=1)
/// untouched (#150 explicit constraint).
#[test]
fn update_forces_certified_values_only_when_genlock_enabled() {
    let src = vendor_file(NDI_SOURCE);
    let body = squish(fn_body(&src, "void ndi_source_update("));

    assert!(
        body.contains("force_genlock_certified_settings(settings)"),
        "{NDI_SOURCE}: #150 — ndi_source_update never calls \
         force_genlock_certified_settings(settings). A genlock source's certified \
         values would not be enforced at the code level. Re-apply the #150 lockdown."
    );
    // The call must be gated on the genlock_fifo flag — never forced unconditionally
    // (that would break the non-genlock aux/preview inputs, #150 constraint #3).
    assert!(
        body.contains("obs_data_get_bool(settings, PROP_GENLOCK_FIFO)"),
        "{NDI_SOURCE}: #150 — ndi_source_update no longer reads PROP_GENLOCK_FIFO to \
         gate the certified forcing. The lockdown must apply ONLY on the genlock path \
         so non-genlock aux/preview inputs keep working. Re-apply the #150 gate."
    );
}

/// #257: `ndi_source_getproperties` is a HARD WHITELIST — it exposes EXACTLY source + Genlock +
/// Latency(ms) + Measurement burn and adds NOTHING else. The forced (non-essential) knobs are
/// REMOVED from the UI entirely (not hidden via the old apply_genlock_lockdown_visibility, which is
/// gone), so a human or a tool CANNOT set them wrong.
#[test]
fn getproperties_is_the_hard_whitelist() {
    let src = vendor_file(NDI_SOURCE);
    let body = squish(fn_body(&src, "obs_properties_t *ndi_source_getproperties("));

    // The old hide-on-lockdown helper must be GONE (replaced by the whitelist).
    assert!(
        !squish(&src).contains("apply_genlock_lockdown_visibility"),
        "{NDI_SOURCE}: #257 — apply_genlock_lockdown_visibility is BACK; the hard whitelist \
         REMOVES the forced knobs from the UI, it does not hide them."
    );
    // getproperties adds EXACTLY the four whitelist props.
    for add in [
        "obs_properties_add_list(props, PROP_SOURCE",
        "obs_properties_add_bool(props, PROP_GENLOCK_FIFO",
        "obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC",
        "obs_properties_add_bool(props, PROP_BURN",
    ] {
        assert!(
            body.contains(add),
            "{NDI_SOURCE}: #257 — the whitelist UI must add `{add}` (source/Genlock/Latency/burn)."
        );
    }
    // None of the forced knobs may be ADDED to the UI (the forcer pins them; the UI never shows them).
    let must_not_add: &[&str] = &[
        "obs_properties_add_list(props, PROP_BEHAVIOR",
        "obs_properties_add_list(props, PROP_BANDWIDTH",
        "obs_properties_add_list(props, PROP_SYNC",
        "obs_properties_add_list(props, PROP_LATENCY",
        "obs_properties_add_bool(props, PROP_FRAMESYNC",
        "obs_properties_add_bool(props, PROP_HW_ACCEL",
        "obs_properties_add_bool(props, PROP_AUDIO",
        "obs_properties_add_list(props, PROP_YUV_RANGE",
        "obs_properties_add_list(props, PROP_YUV_COLORSPACE",
        "obs_properties_add_bool(props, PROP_FIX_ALPHA",
        "obs_properties_add_list(props, PROP_TIMEOUT",
        "obs_properties_add_group(props, PROP_PTZ",
        "obs_properties_add_int_slider(props, PROP_GENLOCK_PRELOAD",
    ];
    for add in must_not_add {
        assert!(
            !body.contains(add),
            "{NDI_SOURCE}: #257 — `{add}` is BACK in the UI; the whitelist exposes only \
             source/Genlock/Latency/burn (everything else is forced, not shown)."
        );
    }
}

/// Lock-step gate: the Windows production-build workflow re-asserts the #150/#257 hard-lock SOURCE
/// tokens in pwsh BEFORE the 150-min build (the Linux Rust guard above can't compile on the
/// windows-2022 runner). Drop the workflow check and CI fails here.
#[test]
fn windows_genlock_workflow_gates_on_the_lockdown_patch() {
    let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));

    assert!(
        wf.contains("force_genlock_certified_settings"),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts the certified-value forcer \
         SOURCE patch — a subtree bump could reship a DistroAV that lets a genlock source come up \
         misconfigured. Re-add the pwsh forcer gate."
    );
    assert!(
        wf.contains("GENLOCK_WHITELIST_PROPS"),
        "{WINDOWS_GENLOCK_WF}: #257 — the production build no longer asserts the GENLOCK_WHITELIST_PROPS \
         hard-lock UI. Re-add the pwsh #257 gate."
    );
}
