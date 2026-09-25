//! issue 1242 — a strih multiview TWIN cell stands for its program scene
//! (`vendor/obs-studio/frontend/components/Multiview.{hpp,cpp}`).
//!
//! strih pulls full-bandwidth NDI only for SHOWN cameras, so its built-in multiview renders the
//! always-connected low-bandwidth twin scene `MV Cam 3` instead of `Cam 3` (rendering `Cam 3` would
//! keep its camera "shown" and connected). Without a mapping the operator's multiview would regress:
//! the tally border never lights (the twin is never on program), the label reads `MV Cam 3`, and a
//! click / double-click puts the LOW-BANDWIDTH twin into preview or straight onto program. The twin
//! scene's private setting `camera_box_multiview_target` (written by scripts/strih_bandwidth_roles.py)
//! names its program scene; the multiview resolves each cell to that target for the tally, the label
//! and the click — and never inc_showing's the target (that would reconnect its camera). Every box that
//! never writes the key (stream, imag, resolume) stays byte-for-byte stock.
//!
//! Source-anchor guard (a FRONTEND change — CI is its first compiler; a lifted copy of the helper was
//! syntax-checked against the real obs.hpp locally). A `git subtree pull` re-importing stock OBS drops
//! the patch and CI fails here.

use std::fs;
use std::path::PathBuf;

const MV_CPP: &str = "vendor/obs-studio/frontend/components/Multiview.cpp";
const MV_HPP: &str = "vendor/obs-studio/frontend/components/Multiview.hpp";
const ROLES_PY: &str = "scripts/strih_bandwidth_roles.py";

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_cell_target_helper_reads_the_role_libs_private_key() {
    let cpp = squish(&read(MV_CPP));
    assert!(
        cpp.contains("static OBSSource multiview_cell_target(obs_source_t *src, obs_data_t *priv)"),
        "{MV_CPP}: issue 1242 — the multiview cell-target helper is gone"
    );
    assert!(
        cpp.contains("obs_data_get_string(priv, \"camera_box_multiview_target\")"),
        "{MV_CPP}: issue 1242 — the helper must read the camera_box_multiview_target private key"
    );
    assert!(
        cpp.contains("if (!target || !obs_source_is_scene(target)) return OBSSource(src);"),
        "{MV_CPP}: issue 1242 — an unknown / non-scene target must fall back to the cell itself"
    );
    // the python role lib writes the SAME key
    assert!(
        read(ROLES_PY).contains("MULTIVIEW_TARGET_KEY = \"camera_box_multiview_target\""),
        "{ROLES_PY}: issue 1242 — the role lib's MULTIVIEW_TARGET_KEY drifted from the C++ literal"
    );
}

#[test]
fn update_records_a_target_per_cell_and_labels_it_without_showing_it() {
    let cpp = squish(&read(MV_CPP));
    let hpp = squish(&read(MV_HPP));
    assert!(
        hpp.contains("std::vector<OBSWeakSource> multiviewTargets;"),
        "{MV_HPP}: issue 1242 — the per-cell target vector is gone"
    );
    let upd = cpp
        .find("obs_source_inc_showing(src);")
        .expect("Multiview::Update no longer shows its scene cells");
    let window = &cpp[upd..(upd + 700).min(cpp.len())];
    assert!(
        window.contains("OBSSource cellTarget = multiview_cell_target(src, data);")
            && window.contains("updatedTargets.emplace_back(OBSGetWeakRef(cellTarget));")
            && window.contains("CreateLabel(obs_source_get_name(cellTarget), h / 3)"),
        "{MV_CPP}: issue 1242 — Update must record each cell's target and label the cell with it"
    );
    assert!(
        !cpp.contains("obs_source_inc_showing(cellTarget)"),
        "{MV_CPP}: issue 1242 — the target must NEVER be shown (it would reconnect its camera)"
    );
    assert!(
        cpp.contains("multiviewTargets = std::move(updatedTargets);"),
        "{MV_CPP}: issue 1242 — Update must publish the target vector"
    );
}

#[test]
fn the_tally_border_and_the_click_follow_the_target() {
    let cpp = squish(&read(MV_CPP));
    assert!(
        cpp.contains("if (tallySrc == programSrc) {")
            && cpp.contains("} else if (tallySrc == previewSrc) {"),
        "{MV_CPP}: issue 1242 — the tally border must follow the cell's target"
    );
    let pos = cpp
        .find("OBSSource Multiview::GetSourceByPosition(int x, int y)")
        .expect("GetSourceByPosition is gone");
    // bound the body at the NEXT Multiview:: member (the squished text has no newlines left)
    let head = "OBSSource Multiview::GetSourceByPosition(".len();
    let rest = &cpp[pos + head..];
    let body = &cpp[pos..pos + head + rest.find("Multiview::").unwrap_or(rest.len())];
    assert!(
        body.contains("OBSSource target = OBSGetStrongRef(multiviewTargets[pos]);")
            && body.contains("return target;"),
        "{MV_CPP}: issue 1242 — a click must select the cell's target, never the low-bandwidth twin"
    );
}
