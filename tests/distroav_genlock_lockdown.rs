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

    // Every certified (key -> value) write the forcer MUST perform. The squished
    // source must contain each exact assignment so a dropped or weakened force fails.
    let must_force: &[&str] = &[
        // sync = 2 (SOURCE_TIMECODE / source timing)
        "obs_data_set_int(settings, PROP_SYNC, PROP_SYNC_NDI_SOURCE_TIMECODE)",
        // behavior = 2 (STOP_RESUME_LAST_FRAME)
        "obs_data_set_int(settings, PROP_BEHAVIOR, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME)",
        // bandwidth = 0 (highest)
        "obs_data_set_int(settings, PROP_BANDWIDTH, PROP_BW_HIGHEST)",
        // latency = 0 (NORMAL)
        "obs_data_set_int(settings, PROP_LATENCY, PROP_LATENCY_NORMAL)",
        // timeout = KEEP_CONTENT
        "obs_data_set_int(settings, PROP_TIMEOUT, PROP_TIMEOUT_KEEP_CONTENT)",
        // yuv range = partial
        "obs_data_set_int(settings, PROP_YUV_RANGE, PROP_YUV_RANGE_PARTIAL)",
        // yuv colorspace = BT.709
        "obs_data_set_int(settings, PROP_YUV_COLORSPACE, PROP_YUV_SPACE_BT709)",
        // hw accel = true
        "obs_data_set_bool(settings, PROP_HW_ACCEL, true)",
        // audio = false
        "obs_data_set_bool(settings, PROP_AUDIO, false)",
        // framesync = false
        "obs_data_set_bool(settings, PROP_FRAMESYNC, false)",
        // alpha-blending fix = false
        "obs_data_set_bool(settings, PROP_FIX_ALPHA, false)",
    ];
    for needle in must_force {
        assert!(
            src.contains(needle),
            "{NDI_SOURCE}: #150 — the certified forcer does not perform `{needle}`. \
             That key could then be left at a saved/UI/default value and misconfigure \
             the genlock zero-loss path. Re-apply the full #150 lockdown."
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

/// `ndi_source_getproperties` MUST hide the non-essential properties when genlock is
/// enabled, leaving ONLY the source selection + genlock toggle + preload visible —
/// so a human or a tool CANNOT set the forced keys wrong. Implemented via a
/// modified-callback on the genlock checkbox plus an initial visibility pass.
#[test]
fn getproperties_hides_nonessential_props_on_genlock() {
    let src = vendor_file(NDI_SOURCE);
    let body = squish(fn_body(&src, "obs_properties_t *ndi_source_getproperties("));

    // A single visibility helper applies the hide/show decision; assert it is wired
    // both initially and from the genlock checkbox's modified-callback.
    assert!(
        body.contains("apply_genlock_lockdown_visibility"),
        "{NDI_SOURCE}: #150 — getproperties no longer applies \
         apply_genlock_lockdown_visibility(...) to hide the non-essential props on the \
         genlock path. The forced keys would still be user-editable in the UI. \
         Re-apply the #150 visibility lockdown."
    );

    // Each non-essential property must be passed to set_visible(..., false)-style
    // hiding via the helper. Assert the helper touches each PROP by name so a
    // dropped one fails. (The helper sets visibility per-prop.)
    let hidden: &[&str] = &[
        "PROP_BEHAVIOR",
        "PROP_BANDWIDTH",
        "PROP_SYNC",
        "PROP_FRAMESYNC",
        "PROP_HW_ACCEL",
        "PROP_LATENCY",
        "PROP_AUDIO",
        "PROP_YUV_RANGE",
        "PROP_YUV_COLORSPACE",
        "PROP_FIX_ALPHA",
        "PROP_TIMEOUT",
    ];
    let helper = squish(fn_body(
        &src,
        "static bool apply_genlock_lockdown_visibility(",
    ));
    for prop in hidden {
        assert!(
            helper.contains(prop),
            "{NDI_SOURCE}: #150 — apply_genlock_lockdown_visibility does not control \
             the visibility of {prop}. On the genlock path it must be hidden so it \
             cannot be set wrong. Re-apply the full #150 visibility lockdown."
        );
    }
    // The two legitimate user knobs must NEVER be hidden by the lockdown.
    assert!(
        !helper.contains("obs_property_set_visible(obs_properties_get(props, PROP_SOURCE), false)")
            && !helper.contains(
                "obs_property_set_visible(obs_properties_get(props, PROP_GENLOCK_PRELOAD), false)"
            ),
        "{NDI_SOURCE}: #150 — the lockdown is hiding a LEGITIMATE user knob \
         (PROP_SOURCE or PROP_GENLOCK_PRELOAD). Those two must always stay visible."
    );
}

/// Lock-step gate: the Windows production-build workflow re-asserts the #150 lockdown
/// SOURCE tokens in pwsh BEFORE the 150-min build (the Linux Rust guard above can't
/// compile on the windows-2022 runner). Drop the workflow check and CI fails here.
#[test]
fn windows_genlock_workflow_gates_on_the_lockdown_patch() {
    let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));

    assert!(
        wf.contains("force_genlock_certified_settings"),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts the #150 \
         certified-value forcer SOURCE patch — a future subtree bump could reship a \
         DistroAV that lets a genlock source come up misconfigured while the version \
         pin still passes. Re-add the pwsh #150 source-patch gate."
    );
    assert!(
        wf.contains("apply_genlock_lockdown_visibility"),
        "{WINDOWS_GENLOCK_WF}: the production build no longer asserts the #150 \
         property-visibility lockdown. Re-add the pwsh #150 source-patch gate."
    );
}
