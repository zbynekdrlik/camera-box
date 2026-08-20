//! #1124 — the three REPORT-ONLY measurement-eq (#1003) diagnostics must stay wired into
//! `scripts/recording-e2e.sh`, and their sourced-lib helpers must exist in
//! `scripts/lib/measurement-eq.sh`. Static-anchor tests (the repo's #328/#675 model): the harness
//! text is read and searched for the exact call/definition shapes — no cargo, no rig.
//!
//! The three items, all REPORT-ONLY (they never touch $GATE; the verdict gate is unchanged):
//!   1. staleness alert          — post-verdict, off the run's all_cambox_delivery_latency block.
//!   2. edge-oscillation classifier — post-verdict, ONLY on a FAILED profile run ($GATE != 0).
//!   3. POST-record stomp re-check  — right after StopRecord, while the pins are STILL in force.
//!
//! Anchors are the CALL-WITH-ARG / `sub.add_parser` forms that appear ONLY at the real site, never
//! in a comment (the #832 self-collision lesson).

use std::fs;
use std::path::PathBuf;

fn read_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn read_harness() -> String {
    read_file("scripts/recording-e2e.sh")
}

/// Item 3 — the post-record stomp re-check call is present, GUARDED by measurement_eq_enabled +
/// ALL_CAMBOX, and lands in the [7/8] `set +e` post-record region (after StopRecord, before the
/// verdict), where the measurement pins are still in force.
#[test]
fn post_record_stomp_recheck_is_wired_in_the_set_e_region_1124() {
    let s = read_harness();
    let call = s
        .find("measurement_eq_post_record_stomp_recheck \"$MEASUREMENT_EQ_PROFILE\"")
        .expect("#1124: the post-record stomp re-check call must be wired into recording-e2e.sh");
    // It must be inside the resilient [7/8] StopRecord region (after the banner, before the verdict).
    let region_start = s
        .find("[7/8]")
        .expect("#1124: the [7/8] StopRecord banner is missing");
    let verdict_at = s
        .find("LIVENESS-GUARDED verdict run")
        .expect("#1124: the verdict-run anchor is missing");
    assert!(
        region_start < call && call < verdict_at,
        "#1124: the stomp re-check must run AFTER StopRecord and BEFORE the verdict (pins still in \
         force; cleanup restores them only at exit) — call at {call}, region [{region_start}, {verdict_at})"
    );
    // Guarded by profile mode + ALL_CAMBOX (never runs on a non-profile / single-camera run).
    let guard = s[..call]
        .rfind("if measurement_eq_enabled && [ \"${ALL_CAMBOX:-0}\" = \"1\" ]; then")
        .expect("#1124: the stomp re-check must be guarded by measurement_eq_enabled + ALL_CAMBOX");
    assert!(
        guard < call,
        "#1124: the guard must precede the stomp re-check call"
    );
}

/// Items 1+2 — the post-verdict staleness + edge-oscillation diagnostics call is present, guarded
/// by measurement_eq_enabled, in the EXECUTE-mode region AFTER the merge computed $GATE and the
/// report rendered (so $GATE and $REPORT_JSON both exist).
#[test]
fn post_verdict_diagnostics_are_wired_after_the_gate_is_decided_1124() {
    let s = read_harness();
    let call = s
        .find("measurement_eq_post_verdict_diagnostics \"$MEASUREMENT_EQ_PROFILE\"")
        .expect("#1124: the post-verdict staleness/edge diagnostics call must be wired in");
    // Runs after the merge that sets $GATE and after the report render (both must precede it).
    let gate_at = s
        .find("\"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\" || GATE=$?")
        .expect("#1124: the merge that decides $GATE is missing");
    let render_at = s
        .find("render the 2-graph report PNG")
        .expect("#1124: the report-render step is missing");
    assert!(
        gate_at < call && render_at < call,
        "#1124: the diagnostics must run AFTER $GATE is decided and the report rendered \
         (report-only, never affecting $GATE) — call at {call}, gate at {gate_at}, render at {render_at}"
    );
    // The call passes $GATE through so item 2 fires only on a failed run.
    assert!(
        s.contains("measurement_eq_post_verdict_diagnostics \"$MEASUREMENT_EQ_PROFILE\" \"$REPORT_JSON\" \"$GATE\""),
        "#1124: the diagnostics call must pass the profile, verdict JSON, and $GATE"
    );
}

/// The sourced lib defines the three helpers the harness calls (the #675 anchor-safe indirection:
/// the harness gains only call lines; the bodies live here, invisible to the harness anchor tests).
#[test]
fn measurement_eq_lib_defines_the_diagnostic_helpers_1124() {
    let lib = read_file("scripts/lib/measurement-eq.sh");
    for def in [
        "measurement_eq_post_record_stomp_recheck() {",
        "measurement_eq_post_verdict_diagnostics() {",
    ] {
        assert!(
            lib.contains(def),
            "#1124: scripts/lib/measurement-eq.sh must define {def}"
        );
    }
    // The stomp re-check reuses the existing verify-measurement-pins command for BOTH roles.
    assert!(
        lib.contains("verify-measurement-pins")
            && lib.contains("--role strih")
            && lib.contains("--role stream"),
        "#1124: the stomp re-check must reuse verify-measurement-pins for both roles"
    );
    // The post-verdict helper drives the two report-only CLI subcommands.
    assert!(
        lib.contains("staleness-from-verdict") && lib.contains("edge-oscillation"),
        "#1124: the post-verdict helper must call staleness-from-verdict + edge-oscillation"
    );
    // Item 2 is gated on a FAILED run ($gate != 0) — a passing run never emits the edge classifier.
    assert!(
        lib.contains("if [ \"$_gate\" != \"0\" ]; then"),
        "#1124: edge-oscillation must fire only when the run failed ($gate != 0)"
    );
}

/// The pure resolver exposes the two report-only subcommands the harness wiring depends on.
#[test]
fn resolver_exposes_the_report_only_subcommands_1124() {
    let py = read_file("scripts/e2e_measurement_pins.py");
    assert!(
        py.contains("\"staleness-from-verdict\"") && py.contains("\"edge-oscillation\""),
        "#1124: e2e_measurement_pins.py must register the staleness-from-verdict + edge-oscillation subcommands"
    );
    for f in [
        "def observed_delivery_from_verdict(",
        "def edge_oscillation_report(",
    ] {
        assert!(
            py.contains(f),
            "#1124: e2e_measurement_pins.py must define {f}"
        );
    }
}
