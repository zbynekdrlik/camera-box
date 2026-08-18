//! Patch-presence guard for #825 — DistroAV `ndi_apply` button vs the OBS 32.2 deprecation.
//!
//! OBS 32.2 marked the 4-arg `obs_properties_add_button(props, name, text, cb)`
//! `OBS_DEPRECATED` (libobs/obs-properties.h) and added the 5-arg
//! `obs_properties_add_button2(props, name, text, cb, void *priv)`. The DistroAV
//! compile-check builds with `-Werror=deprecated-declarations`, so the deprecated call in
//! `vendor/distroav/src/ndi-filter.cpp` FAILS the Linux genlock compile gate (live: run
//! 32138592522, ndi-filter.cpp:159). The fix migrates the "Apply Settings" button to
//! `button2`, passing the filter instance (`ndi_filter_getproperties`'s `void *data`) as
//! the explicit `priv`. That is behaviour-preserving: `obs_property_button_clicked()`
//! hands `p->priv` to the callback exactly as the pre-migration path handed
//! `context->data` (the same filter instance) — see libobs/obs-properties.c. A future
//! DistroAV subtree pull reverts this — this guard then fails loudly.

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const NDI_FILTER: &str = "vendor/distroav/src/ndi-filter.cpp";

#[test]
fn ndi_apply_button_is_off_the_deprecated_4arg_form() {
    let src = vendor_file(NDI_FILTER);
    assert!(
        !src.contains("obs_properties_add_button(props, \"ndi_apply\""),
        "{NDI_FILTER}: #825 — the ndi_apply button is back on the 4-arg \
         obs_properties_add_button that OBS 32.2 deprecated (-Werror=deprecated-declarations \
         fails the Linux compile gate); migrate to obs_properties_add_button2(..., data) \
         (a subtree pull of DistroAV likely reverted it)."
    );
}

#[test]
fn ndi_apply_button_uses_button2_with_the_filter_instance_as_priv() {
    let src = vendor_file(NDI_FILTER);
    assert!(
        src.contains("obs_properties_add_button2(props, \"ndi_apply\""),
        "{NDI_FILTER}: #825 — expected obs_properties_add_button2 for the ndi_apply button."
    );
    // the getproperties param must be named so `data` (the filter instance) exists to pass
    assert!(
        src.contains("ndi_filter_getproperties(void *data)"),
        "{NDI_FILTER}: #825 — ndi_filter_getproperties must name its void* param `data`."
    );
    // and that instance must be handed to button2 as the explicit priv (5th arg)
    assert!(
        src.contains("return true;\n\t\t\t\t  }, data);"),
        "{NDI_FILTER}: #825 — button2 must pass the filter instance (data) as its priv, or the \
         click callback loses its ndi_filter_t*."
    );
}
