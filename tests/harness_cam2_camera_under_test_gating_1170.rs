//! issue 1170 — cam2's CAMERA-UNDER-TEST participation must DERIVE from CAMERA_ACTIVE_SET
//! membership, while its PAINTER role stays UNCONDITIONAL (owner order 2026-08-24).
//!
//! cam2's ShadowCast grabber (which captures imag-nb's HDMI output — cam2's leg measures the imag
//! projection path, issue 781) has a cure-decay that collapsed to ~7 min (issue 1193), so its
//! capture leg cannot survive a 40-min run. The owner chose exclusion-until-card-swap (issue 1198).
//! But before this change cam2's camera-under-test participation was HARDCODED (keyed off
//! PAINTER_IP), not set-derived, so plain set-removal did NOT exclude it. These tests pin the fix:
//!
//!   * camera-set.sh: the default active set drops cam2 (kept cam3); the default sweep + align set
//!     drop cam2; re-adding cam2 to CAMERA_ACTIVE_SET restores its align membership (one-line
//!     reversal — the whole point);
//!   * recording-e2e.sh: the [2b/8] burn deploy seed is gated on `camera_is_active cam2`, and the
//!     [0/8] leg-health preflight skips cam2's (sick) capture leg when cam2 is not a measured
//!     camera.
//!
//! The PAINTER role (reachability preflight, DanteSync clock gate, [3/8] painter launch + QPSK
//! marker, cleanup painter restore) is keyed off PAINTER_IP and is NOT gated by these tests — it
//! stays unconditional by design.
//!
//! RED before the fix: the default set/sweep/align include cam2, the [2b/8] seed is unconditional,
//! and the leg-health loop has no cam2 skip. GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source camera-set.sh (with an optional CAMERA_ACTIVE_SET override) and print the resolved
/// CAMERA_ACTIVE_SET, CAMERA_ALIGN_SET, and camera_active_sweep_pairs — the REAL runtime contract,
/// exactly as every fleet consumer reads them.
fn resolved(active_override: Option<&str>) -> (String, String, String) {
    let script = manifest_dir().join("scripts/camera-set.sh");
    assert!(script.exists(), "{} not found", script.display());
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
printf 'ACTIVE\t%s\n' "$CAMERA_ACTIVE_SET"
printf 'ALIGN\t%s\n' "$CAMERA_ALIGN_SET"
printf 'SWEEP\t%s\n' "$(camera_active_sweep_pairs)"
"#;
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(harness).env("SCRIPT", &script);
    // An override MUST be honoured; the DEFAULT case must NOT inherit this test process's own env.
    match active_override {
        Some(v) => {
            cmd.env("CAMERA_ACTIVE_SET", v);
        }
        None => {
            cmd.env_remove("CAMERA_ACTIVE_SET");
        }
    }
    cmd.env_remove("CAMERA_ALIGN_SET");
    let out = cmd.output().expect("failed to run camera-set.sh harness");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| -> String {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}\t")))
            .unwrap_or("")
            .to_string()
    };
    (field("ACTIVE"), field("ALIGN"), field("SWEEP"))
}

/// Word-exact membership over a space-separated set (never a substring — "cam1" is not in "cam10").
fn has_word(set: &str, word: &str) -> bool {
    set.split_whitespace().any(|w| w == word)
}

#[test]
fn default_active_set_excludes_cam2_keeps_cam3() {
    let (active, _, _) = resolved(None);
    assert!(
        !has_word(&active, "cam2"),
        "issue 1170: cam2 must be dropped from the default CAMERA_ACTIVE_SET (capture leg retired \
         until the card swap). Got ACTIVE=[{active}]"
    );
    assert!(
        has_word(&active, "cam3"),
        "issue 1170: cam3 (the source) must remain the default measured camera. Got ACTIVE=[{active}]"
    );
}

#[test]
fn default_sweep_excludes_cam2_keeps_cam3() {
    let (_, _, sweep) = resolved(None);
    assert!(
        !sweep.contains("Cam 2:CAM2"),
        "issue 1170: the default CAMBOX_SWEEP (camera_active_sweep_pairs) must NOT cut cam2 into \
         strih program — it is no longer a measured camera-under-test. Got SWEEP=[{sweep}]"
    );
    assert!(
        sweep.contains("Cam 3:CAM3"),
        "issue 1170: the default sweep must still cover cam3. Got SWEEP=[{sweep}]"
    );
}

#[test]
fn default_align_set_excludes_cam2_keeps_cam3_and_cam4() {
    // CAMERA_ALIGN_SET stays a superset of the MEASURED set (cam4 is on-air but its capture leg
    // wedges, #947, so it is aligned yet unmeasured). cam2's membership now DERIVES from
    // CAMERA_ACTIVE_SET (issue 1170) — dropped by default, restored on re-add.
    let (_, align, _) = resolved(None);
    assert!(
        !has_word(&align, "cam2"),
        "issue 1170: cam2 must be dropped from the default CAMERA_ALIGN_SET (its capture leg is \
         retired, so it is not aligned). Got ALIGN=[{align}]"
    );
    assert!(
        has_word(&align, "cam3") && has_word(&align, "cam4"),
        "issue 1170: cam3 (source) + cam4 (on-air, #947) stay in the on-air align superset. \
         Got ALIGN=[{align}]"
    );
}

#[test]
fn readding_cam2_to_active_restores_its_align_and_sweep_membership_one_line() {
    // The reversibility the owner mandated: adding "cam2" back to CAMERA_ACTIVE_SET restores its
    // camera-under-test participation everywhere with a single one-line edit, no other change.
    let (active, align, sweep) = resolved(Some("cam2 cam3"));
    assert!(
        has_word(&active, "cam2"),
        "the override must take: ACTIVE=[{active}]"
    );
    assert!(
        has_word(&align, "cam2"),
        "issue 1170 reversal: cam2 back in CAMERA_ACTIVE_SET must flow into CAMERA_ALIGN_SET \
         automatically (derived membership). Got ALIGN=[{align}]"
    );
    assert!(
        sweep.contains("Cam 2:CAM2"),
        "issue 1170 reversal: cam2 back in CAMERA_ACTIVE_SET must flow into the sweep. \
         Got SWEEP=[{sweep}]"
    );
}

#[test]
fn recording_e2e_gates_the_cam2_2b8_burn_deploy_on_active_set_membership() {
    // The [2b/8] deploy seed used to be an UNCONDITIONAL initializer
    // (`CAMBOX_SECONDARY_DEPLOY=("cam2=$PAINTER_IP=$BURN_CAM2_RUN_ID")`). It must now sit inside a
    // `camera_is_active cam2` guard so removing cam2 from CAMERA_ACTIVE_SET actually excludes its
    // camera-under-test deploy (the crux of issue 1170).
    let s = read("scripts/recording-e2e.sh");
    // Slice the [2b/8] region: from the CAMBOX_SECONDARY_DEPLOY init to the ALL_CAMBOX deploy loop
    // header, so the guard assertion is scoped to the seed, not any later use of the variable.
    let start = s.find("CAMBOX_SECONDARY_DEPLOY=()").expect(
        "issue 1170: the [2b/8] deploy list must be initialised EMPTY, then conditionally \
                 seeded — never an unconditional cam2 initializer",
    );
    let end = s[start..]
        .find("for _scn in $(camera_active_secondary_set)")
        .map(|i| start + i)
        .expect("the secondary-set append loop must follow the seed");
    let seed = &s[start..end];
    assert!(
        seed.contains("camera_is_active cam2"),
        "issue 1170: the cam2 [2b/8] burn deploy seed must be guarded by `camera_is_active cam2`. \
         Seed region:\n{seed}"
    );
    assert!(
        seed.contains("cam2=$PAINTER_IP=$BURN_CAM2_RUN_ID"),
        "issue 1170: the cam2 seed (keyed off PAINTER_IP) must still exist, only now guarded. \
         Seed region:\n{seed}"
    );
}

#[test]
fn recording_e2e_leg_health_skips_cam2_when_not_a_measured_camera() {
    // cam2 is a leg-health target only because it is a hardcoded PAINTER reachability entry
    // (PREFLIGHT_TARGETS) that flows into PREFLIGHT_DANTESYNC_LINUX. Its sick capture leg
    // (issue 1193) must not abort the run, so the [0/8] leg-health loop must skip cam2 whenever it
    // is not a measured camera. The painter clock/reachability gates above are untouched.
    let s = read("scripts/recording-e2e.sh");
    let start = s
        .find("[0/8] leg-health preflight")
        .expect("the #1133 leg-health preflight banner must exist");
    let end = s[start..]
        .find("ok: $_lhbox capture leg healthy")
        .map(|i| start + i)
        .expect("the leg-health loop's ok line must bound the region");
    let region = &s[start..end];
    assert!(
        region.contains("camera_is_active cam2"),
        "issue 1170: the leg-health loop must skip cam2 when it is not a measured camera \
         (`[ \"$_lhbox\" = cam2 ] && ! camera_is_active cam2 && continue`). Region:\n{region}"
    );
}
