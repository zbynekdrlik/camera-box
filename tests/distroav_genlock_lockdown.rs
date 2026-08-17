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
//! true, `ndi_behavior` = 2 (STOP_RESUME_LAST_FRAME) -- RE-CERTIFIED #764 (2026-07-15) to
//! `ndi_behavior` = 0 (KEEP_ACTIVE), see tests/distroav_ndi_keepalive_764.rs --
//! `ndi_recv_hw_accel` = true, `ndi_bw_mode` = 0 (highest), `latency` = 0 (NORMAL),
//! `ndi_audio` = false, `ndi_framesync` = false, `ndi_fix_alpha_blending` = false. The two
//! user knobs (`ndi_source_name`, `genlock_preload`) are NEVER forced.
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
const WINDOWS_GENLOCK_FAST_WF: &str = ".github/workflows/windows-genlock-fast.yml";

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
        // #764 (event-critical, 2026-07-15): re-certified from STOP_RESUME_LAST_FRAME to
        // KEEP_ACTIVE -- STOP_RESUME_LAST_FRAME tore the receiver thread down on every hide,
        // paying a full NDI reconnect on every program cut back to that camera. See
        // tests/distroav_ndi_keepalive_764.rs for the full #764 patch guard.
        "{PROP_BEHAVIOR, false, PROP_BEHAVIOR_KEEP_ACTIVE, false}",
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
    // getproperties adds EXACTLY the five whitelist props.
    for add in [
        "obs_properties_add_list(props, PROP_SOURCE",
        "obs_properties_add_bool(props, PROP_GENLOCK_FIFO",
        "obs_properties_add_int(props, PROP_GENLOCK_LATENCY_MS_SRC",
        "obs_properties_add_bool(props, PROP_BURN",
        "obs_properties_add_bool(props, PROP_GENLOCK_MONITOR",
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

/// #257 — the NAMED whitelist table itself must exist (not just its per-prop effects, already
/// covered by `getproperties_is_the_hard_whitelist` above): a future refactor could rename/inline
/// it away and still happen to add the same five props one-off, silently losing the "one
/// auditable COMPLEMENT-of-the-forced-table" property #257 is built on. Mirrors the pwsh
/// production-build gate's own `-notmatch 'GENLOCK_WHITELIST_PROPS'` check
/// (`.github/workflows/windows-genlock.yml`), asserted here against the real vendored source so
/// it runs on every push, not only on a manual `workflow_dispatch`.
#[test]
fn genlock_whitelist_table_is_declared() {
    let squished = squish(&vendor_file(NDI_SOURCE));
    assert!(
        squished.contains("const char *const GENLOCK_WHITELIST_PROPS[] = {"),
        "{NDI_SOURCE}: #257 — the GENLOCK_WHITELIST_PROPS const table declaration is gone. \
         Re-apply the #257 hard-lock UI whitelist as a single named table (the COMPLEMENT of \
         GENLOCK_FORCED_SETTINGS), not one-off obs_properties_add_* calls."
    );
}

/// #257 — the per-source Measurement-burn toggle (`PROP_BURN`, exposed by the whitelist UI above)
/// must actually be APPLIED live at update time, not just present in the UI: `ndi_source_update`
/// must resolve + invoke the runtime-exported `obs_source_set_genlock_burn` setter, or toggling
/// the UI checkbox would be visually present but functionally inert (no OBS restart needed —
/// see the `resolve_set_genlock_burn` doc comment). Mirrors the pwsh production-build gate's own
/// `-notmatch 'obs_source_set_genlock_burn'` check, asserted here against the real vendored
/// source (scoped to `ndi_source_update`'s own body, stronger than the pwsh whole-file substring
/// check) so it runs on every push, not only on a manual `workflow_dispatch`.
#[test]
fn update_applies_the_measurement_burn_toggle_live() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        squish(&src).contains("obs_source_set_genlock_burn"),
        "{NDI_SOURCE}: #257 — obs_source_set_genlock_burn is gone from the file entirely; the \
         per-source Measurement-burn toggle can no longer be applied at runtime. Re-apply the \
         #257 burn-toggle wiring."
    );
    let body = squish(fn_body(&src, "void ndi_source_update("));
    assert!(
        body.contains("resolve_set_genlock_burn()"),
        "{NDI_SOURCE}: #257 — ndi_source_update no longer resolves/applies the runtime \
         obs_source_set_genlock_burn setter. The PROP_BURN UI checkbox would be present but \
         functionally inert (never toggles the actual per-source burn flag). Re-apply the #257 \
         burn-toggle apply call in ndi_source_update."
    );
}

/// #501: the built-in OBS multiview costs ~80ms/render on imag-nb's Linux/OpenGL build because
/// EVERY cell's full-1080p NDI texture upload happens SYNCHRONOUSLY during the multiview's own
/// render (those sources are otherwise idle — the async upload for a source only happens when
/// something actually renders it). Feeding the multiview from LOW-bandwidth NDI receivers instead
/// (~9x cheaper) fits the #276/#278/#293 render-budget decouple back inside the 16.6ms tick. A
/// per-source `genlock_monitor` operator bool (same WHITELIST shape as `genlock_burn`) is the
/// narrow escape hatch: ONLY for a source that will never feed program, force LOW bandwidth
/// instead of the certified HIGHEST — every other certified forcing stays locked.
#[test]
fn force_genlock_certified_settings_has_monitor_source_bandwidth_exception() {
    let src = vendor_file(NDI_SOURCE);
    let squished = squish(&src);

    assert!(
        squished.contains("PROP_GENLOCK_MONITOR"),
        "{NDI_SOURCE}: #501 — no PROP_GENLOCK_MONITOR constant found. Add a `genlock_monitor` \
         per-source operator bool, mirroring the PROP_BURN whitelist pattern."
    );
    assert!(
        squished.contains("PROP_BURN, PROP_GENLOCK_MONITOR,"),
        "{NDI_SOURCE}: #501 — PROP_GENLOCK_MONITOR must be added to GENLOCK_WHITELIST_PROPS \
         (right after PROP_BURN), or the hard-lock UI will never expose the monitor-source \
         toggle to the operator/tooling."
    );

    // The certified forcer must OVERRIDE bandwidth to LOWEST when genlock_monitor is set —
    // narrowly (bandwidth ONLY), never loosening any other certified value.
    let body = squish(fn_body(
        &src,
        "static void force_genlock_certified_settings(",
    ));
    assert!(
        body.contains("obs_data_get_bool(settings, PROP_GENLOCK_MONITOR)"),
        "{NDI_SOURCE}: #501 — force_genlock_certified_settings must read PROP_GENLOCK_MONITOR to \
         decide whether this source is a monitoring-only receiver."
    );
    assert!(
        body.contains("obs_data_set_int(settings, PROP_BANDWIDTH, PROP_BW_LOWEST)"),
        "{NDI_SOURCE}: #501 — when genlock_monitor is set, the forcer must set PROP_BANDWIDTH to \
         PROP_BW_LOWEST (low-bandwidth NDI receive) instead of the certified HIGHEST — this is \
         the whole #501 fix (6x full-1080p uploads during a multiview render → ~9x cheaper)."
    );
    // The DEFAULT certified forcing (highest bandwidth for every OTHER source) must remain — the
    // exception must be ADDITIVE, never a replacement of the base certified table.
    assert!(
        squished.contains("{PROP_BANDWIDTH, false, PROP_BW_HIGHEST, false}"),
        "{NDI_SOURCE}: #501 — the base certified forcing (PROP_BW_HIGHEST for every non-monitor \
         source) must remain in GENLOCK_FORCED_SETTINGS; the monitor exception narrowly OVERRIDES \
         it afterward, it must not replace it."
    );
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

/// #795 — the genlock NDI source selector must be a NON-editable list (`OBS_COMBO_TYPE_LIST`), and
/// the currently-saved source name must always be injected as a selectable list entry so an empty /
/// partial NDI finder can never clobber it.
///
/// Root cause (live event 2026-07-17, stream OBS): `PROP_SOURCE` was an `OBS_COMBO_TYPE_EDITABLE`
/// combo. With the NDI finder list EMPTY on a sick network the operator typed into the combo's line
/// edit, mangling the stored source name character-by-character (`'NDI 2ME PGM' → … → '2ME-PGM'`),
/// each a nonexistent source → black, recoverable only by a full OBS restart (the saved scene JSON
/// still held the correct name). Making the combo list-only removes the free-text surface entirely.
///
/// The saved-name injection is REQUIRED, not optional: `OBSPropertiesView::AddList()`
/// (`vendor/obs-studio/shared/properties-view/properties-view.cpp`) ends with
/// `if (count && idx == -1) info->ControlChanged();` — i.e. for a non-editable LIST combo whose
/// saved value is NOT among the (non-empty) list items, OBS writes the combo's index-0 default back
/// into settings, silently clobbering the stored source name on properties-open. The NDI finder is
/// asynchronous and can momentarily return only OTHER sources, so the saved name must always be a
/// list entry (`idx != -1`) for this writeback never to fire.
#[test]
fn source_selector_is_list_only_with_saved_name_preserved_795() {
    let src = vendor_file(NDI_SOURCE);
    let squished = squish(&src);
    let body = squish(fn_body(&src, "obs_properties_t *ndi_source_getproperties("));

    // The source-selection combo must be LIST (non-editable), not EDITABLE — free text into the
    // source name is exactly the 2026-07-17 black-screen trap.
    assert!(
        body.contains("OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_STRING"),
        "{NDI_SOURCE}: #795 — the PROP_SOURCE selector is not built as a LIST combo \
         (OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_STRING). An editable combo lets an operator type \
         free text into the source name and black-screen the feed (the 2026-07-17 incident)."
    );
    assert!(
        !squished.contains("OBS_COMBO_TYPE_EDITABLE"),
        "{NDI_SOURCE}: #795 — OBS_COMBO_TYPE_EDITABLE is still present. The genlock NDI source \
         selector must be list-only; a free-text combo is the exact trap that mangled the stored \
         source name during a live event. Remove every editable-combo use here."
    );

    // The saved-name preservation helper must exist (defends against the properties-view.cpp
    // `if (count && idx == -1) ControlChanged()` clobber described in the doc comment above).
    assert!(
        squished.contains("genlock_ensure_saved_source_listed"),
        "{NDI_SOURCE}: #795 — the saved-source-name preservation helper \
         `genlock_ensure_saved_source_listed` is missing. Under a LIST combo, a saved source name \
         absent from a non-empty NDI finder list is silently overwritten on properties-open."
    );
    let helper = squish(fn_body(&src, "static void genlock_ensure_saved_source_listed("));
    assert!(
        helper.contains("obs_source_get_settings(s->obs_source)"),
        "{NDI_SOURCE}: #795 — genlock_ensure_saved_source_listed must read the source's own saved \
         settings (obs_source_get_settings(s->obs_source)) to recover the configured source name."
    );
    assert!(
        helper.contains("obs_property_list_add_string"),
        "{NDI_SOURCE}: #795 — genlock_ensure_saved_source_listed must ADD the saved source name to \
         the list (obs_property_list_add_string) so it is a selectable, current entry even when the \
         NDI finder is empty."
    );

    // getproperties must actually CALL the helper (a helper never invoked preserves nothing).
    assert!(
        body.contains("genlock_ensure_saved_source_listed(source_list, s)"),
        "{NDI_SOURCE}: #795 — ndi_source_getproperties never calls \
         genlock_ensure_saved_source_listed(source_list, s); the saved source name would not be \
         injected and a LIST combo could clobber it."
    );
}

/// #795 lock-step gate: BOTH Windows genlock build workflows re-assert the list-only source-selector
/// tokens in pwsh (the Linux Rust guard above can't compile on the windows-2022 runner; the FAST
/// path also ships distroav.dll for hot-swap, so it must gate too — the same lock-step convention
/// #245/#249 established). Drop either check and CI fails HERE.
#[test]
fn windows_genlock_workflows_gate_on_the_list_only_source_selector_795() {
    for wf_path in [WINDOWS_GENLOCK_WF, WINDOWS_GENLOCK_FAST_WF] {
        let wf = squish(&vendor_file(wf_path));
        assert!(
            wf.contains("OBS_COMBO_TYPE_LIST"),
            "{wf_path}: #795 — the build no longer asserts the list-only source selector \
             (OBS_COMBO_TYPE_LIST). A subtree bump could reship an editable source combo (the \
             2026-07-17 black-screen trap). Re-add the pwsh #795 gate."
        );
        assert!(
            wf.contains("genlock_ensure_saved_source_listed"),
            "{wf_path}: #795 — the build no longer asserts the saved-source-name preservation helper \
             (genlock_ensure_saved_source_listed). Re-add the pwsh #795 gate."
        );
    }
}
