//! #1003 — floor-3 per-run camera alignment: wiring + domain-safety anchors.
//!
//! The pure resolver is unit-tested with no rig in tests/python/test_qr_align_pins_1003.py (22
//! tests). These static-anchor tests lock that scripts/recording-e2e.sh actually wires the
//! BLOCKING [4i/8align] step (owner rework mandate 2026-08-20: "zarad ten screenshot spread check
//! aj s auto-align do e2e"), that it is correctly gated + aborts on failure, that the align set
//! includes cam4 (a superset of CAMERA_ACTIVE_SET), and that the DOMAIN boundary holds: the aligner
//! writes strih pins ONLY, never the stream NDI 2ME PGM hold or imag's 3 ms floor.

use std::fs;
use std::path::Path;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source camera-set.sh (with an optional CAMERA_ACTIVE_SET override) and return the RESOLVED
/// `CAMERA_ALIGN_SET` — issue 1170 made cam2's align membership DERIVE from CAMERA_ACTIVE_SET, so
/// the default is a `$(case …)` command substitution, not a bare literal; the contract to pin is
/// the resolved value, not the source text.
fn resolved_align_set(active_override: Option<&str>) -> String {
    let script = format!("{}/scripts/camera-set.sh", env!("CARGO_MANIFEST_DIR"));
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg("set -uo pipefail\n. \"$SCRIPT\"\nprintf '%s' \"$CAMERA_ALIGN_SET\"")
        .env("SCRIPT", &script);
    match active_override {
        Some(v) => {
            cmd.env("CAMERA_ACTIVE_SET", v);
        }
        None => {
            cmd.env_remove("CAMERA_ACTIVE_SET");
        }
    }
    cmd.env_remove("CAMERA_ALIGN_SET");
    let out = cmd.output().expect("failed to source camera-set.sh");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn align_has_word(set: &str, word: &str) -> bool {
    set.split_whitespace().any(|w| w == word)
}

/// The [4i/8align] block, sliced from its banner echo to the following freeze-watch step marker.
/// Both anchors are unique in recording-e2e.sh (verified), so this is the step's own text only.
fn align_block(s: &str) -> &str {
    let start = s
        .find("[4i/8align] #1003 floor-3 per-run camera alignment")
        .expect("[4i/8align] comment header must exist in recording-e2e.sh");
    let end = s
        .find("# #758 item 3 — arm the in-run freeze watch for the WHOLE recording window")
        .expect("the freeze-watch marker (block end) must exist");
    assert!(
        end > start,
        "the align block must sit before the freeze-watch step"
    );
    &s[start..end]
}

#[test]
fn recording_e2e_sh_wires_the_qr_align_step() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("[4i/8align]"),
        "recording-e2e.sh must carry the #1003 [4i/8align] floor-3 alignment step banner."
    );
    let block = align_block(&s);
    assert!(
        block.contains(". \"$HERE/lib/qr-align.sh\"") && block.contains("qr_align_run"),
        "the step must source scripts/lib/qr-align.sh and call qr_align_run (the #675 pattern)."
    );
}

#[test]
fn qr_align_step_is_blocking_aborts_on_failure() {
    let s = read("scripts/recording-e2e.sh");
    let block = align_block(&s);
    // A failure to align ABORTS the run (owner: measure -> align -> verify -> FAIL if it cannot).
    assert!(
        block.contains("exit 1"),
        "the [4i/8align] step must ABORT the run (exit 1) when it cannot align — never proceed \
         to StartRecord on a misaligned rig."
    );
}

#[test]
fn qr_align_step_is_gated_and_skips_under_measurement_eq() {
    let s = read("scripts/recording-e2e.sh");
    let block = align_block(&s);
    // Disableable (QR_ALIGN), ALL_CAMBOX-only, and MUTUALLY EXCLUSIVE with the measurement-eq
    // profile (the OTHER strih-pin writer) — the two can never both write strih pins in one run.
    assert!(
        block.contains("QR_ALIGN")
            && block.contains("ALL_CAMBOX")
            && block.contains("! measurement_eq_enabled"),
        "the step must be gated on QR_ALIGN + ALL_CAMBOX and skip under measurement_eq_enabled."
    );
}

#[test]
fn align_set_is_a_superset_including_cam4_and_cam1_cam2_derive_from_active_1198() {
    // The owner mandate: cam4 is on-air, so it MUST be aligned even though it is excluded from the
    // measurable E2E sweep (CAMERA_ACTIVE_SET). CAMERA_ALIGN_SET stays a superset of the measured
    // set. issue 1170 (2026-08-24) introduced cam2's align membership DERIVING from
    // CAMERA_ACTIVE_SET; issue 1198 (2026-08-27, owner ruling: both cards restored healthy)
    // generalizes the same derivation to cam1. Today's default (cam1 cam2 cam3 all active)
    // resolves to "cam1 cam2 cam3 cam4" -- all FOUR on-air cameras aligned, matching the owner's
    // own live observation that only cam3+cam4 were being aligned while cam1/cam2 sat outside.
    let default_align = resolved_align_set(None);
    assert!(
        align_has_word(&default_align, "cam3") && align_has_word(&default_align, "cam4"),
        "#1003: the default align set must include cam3 (source) + cam4 (on-air, #947): got [{default_align}]"
    );
    assert!(
        align_has_word(&default_align, "cam1"),
        "issue 1198: cam1 must be IN the default align set (card restored healthy 2026-08-27): \
         got [{default_align}]"
    );
    // issue 1216/1152 rig-model correction (2026-08-28, run 33166543288 [4i/8align] evidence):
    // cam2 is the PROJECTION PROBE -- its grabber captures imag-nb's HDMI OUTPUT, so its view of
    // the painter QR arrives through painter -> cam1 camera -> strih -> imag -> HDMI -> grabber,
    // structurally 7-9 painter ids (~120-150 ms) behind the direct splitter family. The floor-3
    // MUTUAL align cannot equalize it by design, and its bimodal decode (4/17, twice-rescaled
    // optical image) flips the measured spread 2-3 <-> 6-9 ids, failing the stability criterion.
    // cam2 therefore NEVER derives into the align set, even while in CAMERA_ACTIVE_SET (its E2E
    // leg + probe role are untouched).
    assert!(
        !align_has_word(&default_align, "cam2"),
        "issue 1216: cam2 (projection probe) must NOT be in the default align set -- its view is \
         structurally ~8 painter frames behind the splitter family: got [{default_align}]"
    );
    let without_either = resolved_align_set(Some("cam3"));
    assert!(
        !align_has_word(&without_either, "cam1") && !align_has_word(&without_either, "cam2"),
        "issue 1198 reversal check: shrinking CAMERA_ACTIVE_SET back to cam3-alone must drop \
         cam1 from the align set again (derived, not hardcoded): got [{without_either}]"
    );
    let with_cam2 = resolved_align_set(Some("cam2 cam3"));
    assert!(
        !align_has_word(&with_cam2, "cam2"),
        "issue 1216: cam2 in CAMERA_ACTIVE_SET must STILL not flow into the align set (probe \
         path, not an alignable view): got [{with_cam2}]"
    );
    let cs = read("scripts/camera-set.sh");
    assert!(
        cs.contains("camera_align_ndi_sources_csv"),
        "camera-set.sh must provide camera_align_ndi_sources_csv (never a literal cam range)."
    );
}

#[test]
fn align_set_extends_to_cam6_cam7_but_not_cam5_when_active_1217() {
    // issue 1216 (2026-08-28): the bigger splitter puts cam5/cam6/cam7 back in the default
    // CAMERA_ACTIVE_SET -- CAMERA_ALIGN_SET's derivation widened with it, appended after the
    // cam1..cam4 base. issue 1217 (same day): cam5's leg turns out to be a DEAD_PORT (flat
    // static frame) -- it is dropped from CAMERA_ACTIVE_SET again, AND from the ALIGN_SET
    // derivation loop itself (unlike cam1/cam2/cam6/cam7, whose align membership derives from
    // CAMERA_ACTIVE_SET, cam5's does not -- aligning a dead signal has no benefit).
    let default_align = resolved_align_set(None);
    for cam in ["cam6", "cam7"] {
        assert!(
            align_has_word(&default_align, cam),
            "issue 1216: {cam} must be in the default CAMERA_ALIGN_SET (bigger splitter fitted): \
             got [{default_align}]"
        );
    }
    assert!(
        !align_has_word(&default_align, "cam5"),
        "issue 1217: cam5 must NOT be in the default CAMERA_ALIGN_SET -- its splitter leg is a \
         DEAD_PORT, aligning it has no benefit: got [{default_align}]"
    );
    // Shrinking CAMERA_ACTIVE_SET back to a set without them must drop cam6/cam7 from the align
    // set too, derived not hardcoded.
    let without_them = resolved_align_set(Some("cam1 cam2 cam3"));
    for cam in ["cam6", "cam7"] {
        assert!(
            !align_has_word(&without_them, cam),
            "issue 1216 reversal check: shrinking CAMERA_ACTIVE_SET must drop {cam} from the \
             align set again: got [{without_them}]"
        );
    }
    // Re-adding cam5 to CAMERA_ACTIVE_SET alone must NOT bring it back into the align set --
    // issue 1217 deliberately un-derives it; the RE-ENABLE procedure adds it to BOTH explicitly.
    let with_cam5_reactivated = resolved_align_set(Some("cam1 cam2 cam3 cam4 cam5 cam6 cam7"));
    assert!(
        !align_has_word(&with_cam5_reactivated, "cam5"),
        "issue 1217: re-adding cam5 to CAMERA_ACTIVE_SET alone must not re-align it (the align \
         loop no longer checks for it at all): got [{with_cam5_reactivated}]"
    );
    // cam4 stays the on-air-but-unmeasured base regardless (issue 1003) -- unaffected by any of
    // the cam5/cam6/cam7 changes.
    assert!(
        align_has_word(&default_align, "cam4"),
        "cam4 must remain in the align set (on-air, #947): got [{default_align}]"
    );
}

#[test]
fn qr_align_lib_and_tool_exist() {
    for p in ["scripts/lib/qr-align.sh", "scripts/qr_align_pins.py"] {
        let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
        assert!(
            Path::new(&path).exists(),
            "{p} must exist (#1003 floor-3 aligner)"
        );
    }
    let lib = read("scripts/lib/qr-align.sh");
    assert!(
        lib.contains("qr_align_run()"),
        "scripts/lib/qr-align.sh must define qr_align_run()."
    );
}

#[test]
fn aligner_never_writes_the_stream_hold_or_imag_floor() {
    // DOMAIN boundary (owner: NEdotýkať stream NDI 2ME PGM hold; imag always 3ms). Enforced two
    // concrete ways, not just prose: (1) the align SET is only "cam<N>" tokens -- never a stream or
    // imag source -- so the aligner is only ever handed strih inputs; (2) the --pins writer REFUSES
    // an underscore/imag-floor-sentinel key.
    // issue 1170: the align default is now a `$(case …)` deriving cam2 from CAMERA_ACTIVE_SET, so
    // parse the RESOLVED set (source camera-set.sh) rather than the raw default text. Assert both the
    // cam2-out default and the cam2-in reversal produce ONLY bare cam names (never a stream/imag src).
    for resolved in [
        resolved_align_set(None),
        resolved_align_set(Some("cam2 cam3")),
    ] {
        assert!(
            !resolved.is_empty(),
            "CAMERA_ALIGN_SET must resolve to a non-empty on-air set"
        );
        for tok in resolved.split_whitespace() {
            assert!(
                tok.starts_with("cam") && tok[3..].chars().all(|c| c.is_ascii_digit()),
                "CAMERA_ALIGN_SET token {tok:?} must be a bare cam name -- never a stream/imag \
                 source (the align set is strih inputs only, #1003 domain boundary). Got [{resolved}]"
            );
        }
    }
    let alp = read("scripts/apply_latency_pins.py");
    assert!(
        alp.contains("src.startswith(\"_\")") && alp.contains("imag floor sentinel"),
        "apply_latency_pins.py --pins must refuse an underscore / imag-floor-sentinel key."
    );
    let e2e = read("scripts/recording-e2e.sh");
    let block = align_block(&e2e);
    assert!(
        block.contains("strih") && !block.contains("--box stream"),
        "the [4i/8align] step aligns strih only; it must never write the stream box."
    );
}
